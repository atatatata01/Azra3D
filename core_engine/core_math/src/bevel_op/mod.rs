//! Offset-based poly edge bevel (v1: chamfer). See BEVEL_PLAN.md for the full gated plan.
//!
//! PURE geometry: reads a `HalfEdgeMesh` through its PUBLIC read API only and rebuilds the whole mesh via
//! `HalfEdgeMesh::from_polygons`. `half_edge.rs` is NEVER touched (it is hard-frozen) — this module
//! supersedes the frozen `HalfEdgeMesh::bevel_edges_ex`, which is a single-segment endpoint-slide chamfer
//! with no corner fill and no profile. The new core: translate each edge by a CONSTANT perpendicular
//! offset within each incident face plane (rails stay parallel for any dihedral), intersect adjacent
//! in-plane offset lines at shared corners, and fill the corner with a real face — then segments +
//! superellipse profile in v2.
//!
//! # Module layout (pure relocation)
//!
//! GOD_FILES_SPLIT_PLAN: former single-file `bevel_op.rs` → `bevel_op/{mod,offsets,weld,rails,corners,caps,clamp,tests}.rs`.
//! Topic files map to BEVEL.md §4 STEPs. Pipeline entry points + remaining helpers live in this `mod.rs`;
//! peeled helpers live in topic modules. Public paths `core_math::bevel_op::*` are unchanged.

// Topic modules (pure relocation seams).
mod weld;
mod offsets;
mod rails;
mod corners;
mod caps;
mod clamp;

// Re-export into this module so pipeline + tests keep unqualified paths.
pub(crate) use weld::WeldMap;
pub(crate) use offsets::*;
pub(crate) use rails::Rail;
pub(crate) use corners::*;
pub(crate) use caps::*;
pub(crate) use clamp::{corner_span_sizes, opposing_pair_caps}; // STEP-1 walk stays in the pipeline (function body)


use glam::Vec3;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use crate::half_edge::{
    HalfEdgeMesh, HalfEdgeId, FaceId, VertexId, HVertex, NO_TWIN, NO_FACE, NO_EDGE,
};

/// Diagnostic from the last [`bevel_edges_offset`] call in this thread (app HUD / toast).
/// `effective_segments` may be lower than requested when a blocking multi-edge corner forces flat
/// (BEVEL.md STEP 2.5 `unroundable_blocking`).
thread_local! {
    static LAST_BEVEL_EFFECTIVE_SEG: Cell<u32> = const { Cell::new(1) };
    static LAST_BEVEL_FORCED_FLAT: Cell<bool> = const { Cell::new(false) };
    /// Requested vs post-clamp width — when clamp is ON, drag can raise the HUD width while
    /// geometry stays stuck at the collide limit (owner: "accel / go wider won't work").
    static LAST_BEVEL_WIDTH_REQ: Cell<f32> = const { Cell::new(0.0) };
    static LAST_BEVEL_WIDTH_EFF: Cell<f32> = const { Cell::new(0.0) };
    /// AUTO_POLYGROUPS: half-open range `[start, end)` of **new** faces (strips/caps/seals)
    /// in the rebuilt mesh. `end == 0` means unknown / no-op.
    /// Index in the new-face range where CORNER FILL begins; everything from
    /// `LAST_BEVEL_NEW_FACE_START` up to here is STRIP geometry (see `last_bevel_strip_range`).
    static LAST_BEVEL_CAP_START: Cell<u32> = const { Cell::new(0) };
    static LAST_BEVEL_NEW_FACE_START: Cell<u32> = const { Cell::new(0) };
    static LAST_BEVEL_NEW_FACE_END: Cell<u32> = const { Cell::new(0) };
}

/// Effective segment count used by the last edge bevel on this thread.
pub fn last_bevel_effective_segments() -> u32 {
    LAST_BEVEL_EFFECTIVE_SEG.with(|c| c.get())
}

/// True if the last edge bevel forced segments→1 despite a higher request (can't round yet).
pub fn last_bevel_forced_flat() -> bool {
    LAST_BEVEL_FORCED_FLAT.with(|c| c.get())
}

/// True if clamp-overlap reduced the width on the last edge bevel (geometry stuck while HUD may grow).
pub fn last_bevel_width_was_clamped() -> bool {
    let req = LAST_BEVEL_WIDTH_REQ.with(|c| c.get());
    let eff = LAST_BEVEL_WIDTH_EFF.with(|c| c.get());
    req > 1e-6 && eff + 1e-4 < req
}

/// Post-clamp width used by the last edge bevel (equals request when clamp is off or not binding).
pub fn last_bevel_effective_width() -> f32 {
    LAST_BEVEL_WIDTH_EFF.with(|c| c.get())
}

/// Half-open face range of the STRIP geometry from the last bevel — the band running ALONG
/// the beveled edge, excluding the corner fill at its ends. `None` when unrecorded.
///
/// Smart Select tags this band's edges (its interior segment edges AND its long rims, both
/// tangent to the round) while leaving the corner fill's edges alone, because those are real
/// corners the user still wants beveled.
pub fn last_bevel_strip_range() -> Option<(u32, u32)> {
    let s = LAST_BEVEL_NEW_FACE_START.with(|c| c.get());
    let e = LAST_BEVEL_CAP_START.with(|c| c.get());
    if e > s { Some((s, e)) } else { None }
}

/// Half-open face index range `[start, end)` of **new** geometry from the last bevel rebuild
/// (strips / caps / seals — not re-traced originals). Empty when no-op / unrecorded.
pub fn last_bevel_new_face_range() -> Option<(u32, u32)> {
    let s = LAST_BEVEL_NEW_FACE_START.with(|c| c.get());
    let e = LAST_BEVEL_NEW_FACE_END.with(|c| c.get());
    if e > s {
        Some((s, e))
    } else {
        None
    }
}

/// Record an EXTERNAL bevel-producing op (bevel_absorb's fuse path) in the last-bevel
/// telemetry: the width was achieved exactly (no clamp), and there are NO strip/new-face
/// ranges — without this, the drag HUD and Smart Select's strip tagging would read the STALE
/// record left by a previous `bevel_edges_offset` call and mis-tag unrelated faces.
pub fn record_external_bevel(width: f32, segments: u32) {
    LAST_BEVEL_EFFECTIVE_SEG.with(|c| c.set(segments.max(1)));
    LAST_BEVEL_FORCED_FLAT.with(|c| c.set(false));
    LAST_BEVEL_WIDTH_REQ.with(|c| c.set(width));
    LAST_BEVEL_WIDTH_EFF.with(|c| c.set(width));
    LAST_BEVEL_NEW_FACE_START.with(|c| c.set(0));
    LAST_BEVEL_NEW_FACE_END.with(|c| c.set(0));
    LAST_BEVEL_CAP_START.with(|c| c.set(0));
}

/// Bevel parameters. `width` + `segments` + `shape` drive the profile (seg=1 = flat chamfer).
#[derive(Clone, Copy, Debug)]
pub struct BevelParams {
    /// Offset distance applied perpendicular to the edge within each incident face plane.
    pub width: f32,
    /// Number of profile segments. 1 = flat chamfer; >1 rounds when the corner topology allows.
    pub segments: u32,
    /// Profile shape [Codex P2]: 0 = flat chamfer, 0.5 = circular fillet (Blender superellipse default),
    /// 1 = convex bulge. Blended flat↔circular↔convex in STEP 2.5.
    pub shape: f32,
    /// Clamp width so an oversized bevel can't invert/degenerate a face.
    pub clamp: bool,
}

impl Default for BevelParams {
    fn default() -> Self { Self { width: 0.1, segments: 1, shape: 0.5, clamp: true } }
}

/// How each edge's TARGET width is chosen under a [`BevelWidthPolicy`]. The per-edge CEILING is not
/// this enum's business — a policy always applies the ceiling per edge, whichever driver is used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BevelWidthDriver {
    /// NOT graded — every edge targets `params.width` and only the per-edge CEILING differs, so a
    /// tight pair pins at its meet point (where the [`WeldMap`] merges it) while every other edge
    /// reaches the width the user asked for. `width_min` is unused.
    ///
    /// This is the Bevel modifier's **Auto Merge Vertices**. It exists because the alternative — one
    /// GLOBAL minimum over the whole selection — lets a single tight spot throttle the entire mesh
    /// (measured: a cube with a 0.06-wide loop-cut band bevels at 0.030 everywhere when 0.2 was
    /// requested; per-edge, the roomy corners reach the full 0.200).
    #[default]
    Uniform,
    /// AUTOMATIC and graded — the edge's own local ROOM (the per-edge clamp capacity that STEP 1's
    /// `geometry_collide_offset` walk already computes), normalised by the roomiest edge in the
    /// selection. Small detail scales itself down; nothing has to be authored.
    AutoSize,
    /// AUTHORED and graded — the user's per-edge weight in `0..=1` (Blender's bevel weight).
    /// Unweighted edges read 0.0, i.e. `width_min`.
    AuthoredWeight,
    /// AUTOMATIC and graded by the **corner span** (owner 2026-08-11). Same grading role as
    /// [`Self::AutoSize`], different measuring stick — see [`corner_span_sizes`].
    ///
    /// `AutoSize` reads each edge's clamp CAPACITY, which is a property of one segment. The owner's
    /// complaint: that is not the size of the *detail*. Cut a 2 m panel into ten segments and every
    /// segment reads as small detail, so the same unchanged shape asks for a smaller bevel.
    /// `CornerSpan` instead measures across the corner at the edge's endpoints, with the slide capped
    /// by the SHORTER neighbour — so a 2 m edge meeting a 1 cm edge still reads small and still drives
    /// a small bevel, which is the case the owner called out.
    CornerSpan,
}

impl BevelWidthDriver {
    /// True when the driver spreads widths across the `[width_min, params.width]` band. `Uniform`
    /// does not, so `width_min` must be ignored for it.
    pub fn is_graded(self) -> bool {
        !matches!(self, BevelWidthDriver::Uniform)
    }
}

/// PER-EDGE WIDTH POLICY — `None` at every call site is exactly today's single-global-width bevel.
///
/// TWO independent things are wrong with one width and one global limit, and a policy fixes both:
///
///  * **the CEILING is global.** STEP 1 computes a real per-edge capacity and then collapses it with
///    `cap.values().min()`, so the single tightest edge in a selection freezes every other edge
///    (clamp ON) or the tight spot folds (clamp OFF). A policy applies each edge's own capacity, so a
///    colliding pair pins at its meet point — which is where the [`WeldMap`] merges the two rails
///    into ONE vertex, the "auto merge" behaviour, already clean and watertight at every segment
///    count measured. Use [`BevelWidthDriver::Uniform`] for exactly this and nothing else.
///  * **the TARGET is uniform.** On a model mixing big panels with small detail one width is wrong
///    everywhere; `AutoSize` / `AuthoredWeight` grade it inside `[width_min, params.width]`.
///
/// Not a new geometric regime: per-edge widths already exist and are oracle-gated — `adjust_offsets`
/// produces them (0.2324→0.2562 across one fillet vs Blender's 0.2331→0.2549, BEVEL.md §11
/// 2026-07-23(d)) and they ride [`Rail::w`] through the corner/boundary solvers. This only changes
/// where those numbers come from, so it composes with `adjust_offsets` rather than fighting it.
#[derive(Clone, Debug, Default)]
pub struct BevelWidthPolicy {
    /// SMALL end of the band (`params.width` is the LARGE end). Clamped into `0..=params.width`.
    /// `0.0` = purely proportional. **Ignored unless the driver `is_graded()`.**
    ///
    /// A floor is a TARGET, never a guarantee: the per-edge capacity still wins (§5 invariant 9), so
    /// a floor wider than an edge's room degrades to that room instead of folding the face.
    pub width_min: f32,
    pub driver: BevelWidthDriver,
    /// Replace the ONE global clamp limit with each edge's own capacity.
    ///
    /// `true` for Variable Width and Auto Merge — that substitution IS their purpose, and grading
    /// is meaningless under a global minimum that caps every edge to the tightest one.
    ///
    /// `false` when the policy exists only to apply authored [`Self::weights`]. Scaling a width by a
    /// percentage must not also change WHICH limit governs it: on a mesh whose global limit is the
    /// binding constraint, silently switching to per-edge ceilings makes a 50% weight come out
    /// WIDER than the unweighted bevel (measured: 0.15 against 0.05). The weight then reads as
    /// "turned some other option on", which is exactly the coupling the owner asked to remove.
    pub per_edge_limit: bool,
    /// AUTHORED per-edge weight: half-edge id → `0..=1`, **a PERCENTAGE OF THE WIDTH the driver
    /// chose for that edge** (owner 2026-08-11: "the weight is a percentage of the width").
    /// Key BOTH twins; lookups try the primary id and its twin.
    ///
    /// **An edge with no entry is 1.0 — full width, not zero.** The weight scales a bevel down from
    /// what it would otherwise be, so a mesh with nothing painted bevels exactly as it does with no
    /// policy at all, and painting an edge is what makes it differ. Reading absent as 0.0 would mean
    /// enabling the feature erases the bevel from every edge the user has not painted yet.
    ///
    /// Applied on TOP of the driver rather than being one of the drivers: "which edges did I paint
    /// down" and "how should unpainted widths be graded" are independent questions, and making the
    /// weight a driver forced the user to give up auto-grading to use it.
    pub weights: HashMap<u32, f32>,
}

/// INNER MITER mode (Blender's Bevel "Miter Inner"). Governs the corner where two beveled edges meet
/// at a **concave** vertex. Blender's Miter OUTER is a separate control for CONVEX corners and is a
/// no-op on concave ones (oracle-verified, BEVEL.md §11 2026-08-11) — do not conflate them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MiterInner {
    /// Today's behaviour and Blender's default: the two edges' rails meet at ONE point per profile
    /// level, so the corner is a single arc in the bisector plane. We are vertex-EXACT with Blender
    /// here (0.0000000 max deviation, §11 2026-08-11).
    #[default]
    Sharp,
    /// The sharp meet point is replaced by a circular arc of radius `spread`, swept around the
    /// corner. See [`inner_miter_arc`] for the closed form and BEVEL.md §3.8 for the derivation.
    Arc,
}

/// Inner-miter spec. `None` at every call site is exactly [`MiterInner::Sharp`], i.e. today's bevel.
///
/// `spread` is NOT optional decoration: it sizes the arc, and Blender likewise only exposes Spread
/// once a miter is set to Arc. A Sharp/Arc toggle without it cannot reproduce the reference.
#[derive(Clone, Copy, Debug)]
pub struct BevelMiter {
    pub inner: MiterInner,
    /// How far back along each beveled edge the arc's ends sit from the sharp meet point. Blender's
    /// default is 0.1. Clamped `>= 0`; `0.0` degenerates to Sharp (both ends collapse onto `M`).
    pub spread: f32,
}

impl Default for BevelMiter {
    fn default() -> Self {
        Self { inner: MiterInner::Sharp, spread: 0.1 }
    }
}

// WeldMap → weld.rs (pure relocation)

// Rail → rails.rs (pure relocation)

/// Build a beveled copy of `mesh`. v1 = single-segment offset chamfer (parallel rails + corner patch),
/// rebuilt via `from_polygons`. Returns the input unchanged for a non-positive width or empty selection.
/// VERTEX bevel (Blender `affect='VERTICES'`): truncate/round each selected vertex. Each incident edge
/// gets a BoundVert slid from V along the edge by the offset; consecutive BoundVerts are bridged by a
/// corner arc across each original face (a chord at seg=1, a circular profile bulging toward V at seg>1);
/// on a **closed** vertex the BoundVert ring is capped — a flat n-gon at seg=1, the M_ADJ grid at seg>1.
/// On an **open** vertex (plane corner / open rim — `vertex_fan` is incomplete at a boundary) there is no
/// 3D cap: each incident face simply replaces the V-corner with that face's arc, which becomes the new
/// boundary of the open mesh (Blender plane-corner vertex bevel). Each original face at V has its
/// V-corner replaced by that face's arc (so a plane quad corner → pentagon at seg=1), welded to the cap
/// when closed → watertight. Edges stay sharp; only the vertex is beveled.
///
/// `selected` = vertex ids. v1 scope: each selected vertex is handled independently — for two ADJACENT
/// selected verts (sharing an edge) the shared edge just gets a BoundVert from each end (clamp keeps them
/// from crossing); the vertex-only "double bevel weld" of that edge is not special-cased.
pub fn bevel_vertices_offset(mesh: &HalfEdgeMesh, selected: &HashSet<u32>, params: &BevelParams) -> HalfEdgeMesh {
    LAST_BEVEL_NEW_FACE_START.with(|c| c.set(0));
    LAST_BEVEL_NEW_FACE_END.with(|c| c.set(0));
    if params.width <= 0.0 || selected.is_empty() {
        return mesh.clone();
    }
    let orig_pos: Vec<Vec3> = mesh.vertices.iter().map(|v| v.position).collect();
    let n_seg = (params.segments.max(1)).min(32) as usize;
    let shape = params.shape.clamp(0.0, 1.0);
    let mut w = params.width.max(0.0);
    // Clamp overlap: half the shortest edge incident to any selected vertex (so no slide passes its
    // neighbour). Simpler than the edge-bevel `geometry_collide_offset` — vertex bevel has no strips.
    // Use face-outgoings (not `vertex_fan`) so open-rim / plane corners see both legs of the corner.
    if params.clamp {
        let mut mx = f32::INFINITY;
        for &v in selected {
            let vp = orig_pos[v as usize];
            for &h in &vertex_face_outgoings(mesh, VertexId(v)) {
                let far = orig_pos[mesh.he(h).vertex.0 as usize];
                let len = (far - vp).length();
                if len > 1e-6 {
                    mx = mx.min(len);
                }
                // The face-prev leg is the other edge of the corner (needed when fan is incomplete).
                let pe = mesh.he(h).prev;
                if pe != HalfEdgeId(u32::MAX) && pe != NO_EDGE {
                    let pe_far = orig_pos[mesh.he_source(pe).0 as usize];
                    let plen = (pe_far - vp).length();
                    if plen > 1e-6 {
                        mx = mx.min(plen);
                    }
                }
            }
        }
        if mx.is_finite() {
            w = w.min(mx * 0.5);
        }
    }
    let mut weld = WeldMap::new();
    let mut orig_idx: HashMap<u32, usize> = HashMap::new();
    // Arc across each face-corner at a selected vertex, keyed by (v, outgoing half-edge). Runs pe-side →
    // ce-side (the face is traced pe→V→ce). Length n_seg+1. SHARED by the cap rim and the face re-trace.
    let mut arc_of: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    let mut faces_out: Vec<Vec<usize>> = Vec::new();
    // Parallel material_id per faces_out entry — restored after from_polygons (else materials vanish).
    let mut faces_mats: Vec<u32> = Vec::new();

    // Per selected vertex: build BoundVerts + per-face arcs, then (if closed) cap the ring.
    let mut sel_sorted: Vec<u32> = selected.iter().copied().collect();
    sel_sorted.sort_unstable(); // deterministic weld order
    for &v in &sel_sorted {
        let vp = orig_pos[v as usize];
        let vid = VertexId(v);
        // Face-outgoing HEs at V — complete on open meshes (plane corners). `vertex_fan` alone is
        // boundary-incomplete (stops at first NO_TWIN) and used to skip every plane corner as d<3.
        let face_out = vertex_face_outgoings(mesh, vid);
        if face_out.is_empty() {
            continue; // wire / isolated vert — nothing to bevel
        }
        let closed = vertex_radial_closed(mesh, vid);
        let cap_mat = vertex_bevel_material(mesh, v);
        // BoundVert per face-outgoing edge: V slid toward the edge's far vertex by w.
        let bv_pos = |h: HalfEdgeId| -> Vec3 {
            let far = orig_pos[mesh.he(h).vertex.0 as usize];
            vp + (far - vp).normalize_or_zero() * w
        };
        // Each face at V is traced pe→V→ce (ce outgoing on the face, pe = its face-prev). Build that
        // face's arc from BV(pe-edge) to BV(ce-edge), bulging toward V. Store welded, keyed by (v, ce).
        for &ce in &face_out {
            let pe = mesh.he(ce).prev;
            if pe == HalfEdgeId(u32::MAX) || pe == NO_EDGE || mesh.he(ce).face == NO_FACE {
                continue;
            }
            // BV on the incoming (pe) edge = slide toward pe's SOURCE (pe runs src→V).
            let pe_far = orig_pos[mesh.he_source(pe).0 as usize];
            let a_pe = vp + (pe_far - vp).normalize_or_zero() * w;
            let a_ce = bv_pos(ce);
            let arc: Vec<usize> = (0..=n_seg)
                .map(|i| {
                    let t = i as f32 / n_seg as f32;
                    let p = if n_seg == 1 {
                        a_pe.lerp(a_ce, t)
                    } else {
                        circular_arc(a_pe, a_ce, vp, t)
                    };
                    weld.push(p)
                })
                .collect();
            arc_of.insert((v, ce.0), arc);
        }
        // CAP only for closed vertices (full radial cycle). Open plane corners / rim verts have no
        // 3D truncation face — the face re-trace alone cuts the corner (Blender affect=VERTICES on a
        // plane). Capping an open rim folded a lid over the hole (rim_vertex_bevel gate class).
        if !closed {
            continue;
        }
        // Cap the BoundVert ring. Chain the per-face arcs into the rim loop (each face's ce-side BV equals
        // the next face's pe-side BV → shared). Use fan order when closed (CCW, reliable).
        let fan = mesh.vertex_fan(vid);
        let mut rim: Vec<usize> = Vec::new();
        let mut bv_set: HashSet<usize> = HashSet::new();
        for &ce in &fan {
            if let Some(a) = arc_of.get(&(v, ce.0)) {
                if let (Some(&a0), Some(&a1)) = (a.first(), a.last()) {
                    bv_set.insert(a0);
                    bv_set.insert(a1);
                }
                append_chained(a, &mut rim);
            }
        }
        dedup_ring(&mut rim);
        if rim.len() < 3 {
            continue;
        }
        // Orient the rim so the cap faces OUTWARD (vertex normal). Newell of the rim vs the vertex normal.
        let vn = mesh.compute_vertex_normal(vid);
        let nl = newell_idx(&rim, &weld.positions);
        if nl.dot(vn) < 0.0 {
            rim.reverse();
        }
        if n_seg == 1 {
            faces_out.push(rim); // flat truncation: one n-gon over the BoundVert ring
            faces_mats.push(cap_mat);
        } else {
            // Rounded: the M_ADJ grid, same as an edge-bevel clean apex. Fit the fillet sphere (rim lies on
            // it), place the pole, fill. Falls back to a fan if the rim isn't the clean count×seg chain.
            let nb = rim.len();
            let pts: Vec<Vec3> = rim.iter().map(|&i| weld.positions[i]).collect();
            let rc: Vec3 = pts.iter().copied().sum::<Vec3>() / nb as f32;
            let up = (vp - rc).normalize_or_zero();
            let (mut s_hh, mut s_h, mut s_hu, mut s_u) = (0.0f32, 0.0, 0.0, 0.0);
            for p in &pts {
                let dd = *p - rc;
                let h = dd.dot(up);
                let u = (dd - up * h).length_squared() + h * h;
                s_hh += h * h;
                s_h += h;
                s_hu += h * u;
                s_u += u;
            }
            let nf = nb as f32;
            let det = 4.0 * s_hh * nf - 4.0 * s_h * s_h;
            let (sphere, pole) = if det.abs() > 1e-9 {
                let c_h = (nf * 2.0 * s_hu - 2.0 * s_h * s_u) / det;
                let k = (4.0 * s_hh * s_u - 2.0 * s_h * 2.0 * s_hu) / det;
                let r2 = k + c_h * c_h;
                if r2 > 1e-9 {
                    let c = rc + up * c_h;
                    (Some((c, r2.sqrt())), c + up * r2.sqrt())
                } else {
                    (None, rc + up * w)
                }
            } else {
                (None, rc + up * w)
            };
            if !fill_corner_grid_madj(
                &rim,
                n_seg,
                vp,
                pole,
                sphere,
                shape,
                &bv_set,
                &mut weld,
                &mut faces_out,
            ) {
                let ci = weld.push(pole);
                for i in 0..nb {
                    faces_out.push(vec![rim[i], rim[(i + 1) % nb], ci]);
                    faces_mats.push(cap_mat);
                }
            } else {
                pad_face_mats(&faces_out, &mut faces_mats, cap_mat);
            }
        }
    }
    // Vertex bevel: caps first (new), then re-traced faces. New range = [0, caps_end).
    let vertex_new_end = faces_out.len() as u32;

    // Re-trace faces: unchanged faces copy verbatim; a face touching a selected vertex replaces that
    // vertex's corner with the face's arc (welded — shared with the cap when closed → watertight).
    // Open plane corners rely on this alone (no cap) — do NOT gate on fan length.
    for i in 0..mesh.faces.len() {
        let fid = FaceId(i as u32);
        if !mesh.is_face_live(fid) {
            continue;
        }
        let hes = mesh.face_half_edges(fid);
        if hes.len() < 3 {
            continue; // freed slot / degenerate
        }
        let mat = face_material_id(mesh, fid);
        let m = hes.len();
        let mut ring: Vec<usize> = Vec::new();
        for j in 0..m {
            let ce = hes[j];
            let v = mesh.he_source(ce).0;
            if selected.contains(&v) {
                if let Some(a) = arc_of.get(&(v, ce.0)) {
                    for &ix in a {
                        if ring.last() != Some(&ix) {
                            ring.push(ix);
                        }
                    }
                    continue;
                }
            }
            let idx = *orig_idx
                .entry(v)
                .or_insert_with(|| weld.push(orig_pos[v as usize]));
            if ring.last() != Some(&idx) {
                ring.push(idx);
            }
        }
        while ring.len() >= 2 && ring.first() == ring.last() {
            ring.pop();
        }
        if ring.len() >= 3 {
            faces_out.push(ring);
            faces_mats.push(mat);
        }
    }

    LAST_BEVEL_NEW_FACE_START.with(|c| c.set(0));
    LAST_BEVEL_NEW_FACE_END.with(|c| c.set(vertex_new_end));
    let mut out = HalfEdgeMesh::from_polygons(&weld.positions, &faces_out);
    apply_face_materials(&mut out, &faces_mats);
    out
}

/// True when the radial twin→next walk around `v` returns to its start (closed solid corner).
/// Open-rim / plane corners hit `NO_TWIN` and return false — no 3D cap for those.
fn vertex_radial_closed(mesh: &HalfEdgeMesh, v: VertexId) -> bool {
    let start = mesh.vert(v).half_edge;
    if start == NO_EDGE || !mesh.is_edge_live(start) {
        return false;
    }
    let mut cur = start;
    let mut n = 0usize;
    let max = mesh.half_edges.len();
    loop {
        let twin = mesh.he(cur).twin;
        if twin == NO_TWIN || !mesh.is_edge_live(twin) {
            return false;
        }
        cur = mesh.he(twin).next;
        n += 1;
        if cur == start {
            return n >= 3;
        }
        if n > max || cur == NO_EDGE || !mesh.is_edge_live(cur) {
            return false;
        }
    }
}

/// All live half-edges leaving `v` that bound a face. On closed verts this matches the face-
/// bearing subset of `vertex_fan`. On open / plane verts `vertex_fan` is boundary-incomplete
/// (stops at the first `NO_TWIN`), so a full scan is required — otherwise a plane corner only
/// sees one leg and was wrongly rejected as "degree < 3".
fn vertex_face_outgoings(mesh: &HalfEdgeMesh, v: VertexId) -> Vec<HalfEdgeId> {
    if vertex_radial_closed(mesh, v) {
        return mesh
            .vertex_fan(v)
            .into_iter()
            .filter(|&h| mesh.he(h).face != NO_FACE)
            .collect();
    }
    let mut out = Vec::new();
    for i in 0..mesh.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if !mesh.is_edge_live(h) {
            continue;
        }
        if mesh.he(h).face == NO_FACE {
            continue;
        }
        if mesh.he_source(h) != v {
            continue;
        }
        out.push(h);
    }
    // Deterministic (scan order is allocation order; sort by dest id for stability across rebuilds).
    out.sort_by_key(|&h| mesh.he(h).vertex.0);
    out
}

/// Build a beveled copy of `mesh` (clone + in-place install). Prefer
/// [`bevel_edges_offset_mut`] when the caller already owns a mutable mesh.
pub fn bevel_edges_offset(mesh: &HalfEdgeMesh, selected: &HashSet<u32>, params: &BevelParams) -> HalfEdgeMesh {
    bevel_edges_offset_var(mesh, selected, params, None)
}

/// [`bevel_edges_offset`] with an optional [`BevelWidthPolicy`] (`None` == that function exactly).
pub fn bevel_edges_offset_var(
    mesh: &HalfEdgeMesh,
    selected: &HashSet<u32>,
    params: &BevelParams,
    var: Option<&BevelWidthPolicy>,
) -> HalfEdgeMesh {
    let mut out = mesh.clone();
    bevel_edges_offset_mut(&mut out, selected, params, var);
    out
}

/// [`bevel_edges_offset_var`] plus the INNER MITER spec (§3.8). `miter = None`, or any
/// [`MiterInner::Sharp`] / zero-spread spec, takes the byte-identical Sharp path — the arc code is
/// gated off entirely, so every pre-existing gate is unaffected.
///
/// The miter rides a separate entry point rather than a [`BevelParams`] field on purpose: `params`
/// is built with a struct literal at 163 sites, and the optional-extra-argument shape is already
/// this module's convention (`bevel_edges_offset` → `_var`).
pub fn bevel_edges_offset_full(
    mesh: &HalfEdgeMesh,
    selected: &HashSet<u32>,
    params: &BevelParams,
    var: Option<&BevelWidthPolicy>,
    miter: Option<&BevelMiter>,
) -> HalfEdgeMesh {
    let mut out = mesh.clone();
    bevel_edges_offset_mut_full(&mut out, selected, params, var, miter);
    out
}

/// In-place edge bevel — same geometry as the rebuild path, but only deletes/replaces
/// **affected** faces and appends strip/cap faces. Untouched element IDs stay put.
///
/// `var`: see [`BevelWidthPolicy`]. `None` keeps the single global width — including the exact
/// `adjust_offsets` arithmetic, so every pre-existing gate is bit-identical.
pub fn bevel_edges_offset_mut(
    mesh: &mut HalfEdgeMesh,
    selected: &HashSet<u32>,
    params: &BevelParams,
    var: Option<&BevelWidthPolicy>,
) {
    bevel_edges_offset_mut_full(mesh, selected, params, var, None)
}

/// [`bevel_edges_offset_mut`] with the INNER MITER spec — see [`bevel_edges_offset_full`].
pub fn bevel_edges_offset_mut_full(
    mesh: &mut HalfEdgeMesh,
    selected: &HashSet<u32>,
    params: &BevelParams,
    var: Option<&BevelWidthPolicy>,
    miter: Option<&BevelMiter>,
) {
    // Normalise to "is the arc actually requested" once. Sharp and zero spread are the same thing
    // (§3.8: spread 0 collapses both arc ends onto the sharp meet), and both must take the untouched
    // path rather than a degenerate arc.
    let miter_arc: Option<f32> = miter.and_then(|m| {
        (m.inner == MiterInner::Arc && m.spread > 1e-6).then_some(m.spread)
    });
    LAST_BEVEL_EFFECTIVE_SEG.with(|c| c.set(params.segments.max(1).min(32)));
    LAST_BEVEL_FORCED_FLAT.with(|c| c.set(false));
    LAST_BEVEL_NEW_FACE_START.with(|c| c.set(0));
    LAST_BEVEL_NEW_FACE_END.with(|c| c.set(0));
    if params.width <= 0.0 || selected.is_empty() {
        return;
    }

    // ---------------------------------------------------------------------------------------------
    // Canonicalize the selection to a set of PRIMARY (undirected) beveled half-edges, skipping
    // boundary / NO_FACE edges (they have no second face to offset against — v1 leaves them alone).
    // A directed half-edge is "beveled" iff its undirected edge is selected and manifold.
    // ---------------------------------------------------------------------------------------------
    let n_he = mesh.half_edges.len();
    let mut beveled_he: HashSet<u32> = HashSet::new(); // every directed half-edge whose edge is beveled
    let mut primary_edges: Vec<u32> = Vec::new();      // one representative half-edge per beveled edge
    let mut seen: HashSet<u32> = HashSet::new();
    // Iterate the selection in a STABLE (sorted) order so `primary_edges` — which drives arc/strip/vertex
    // emission order and hence output vertex/face IDs — is deterministic regardless of HashSet iteration
    // order (reproducibility, selection continuity, test stability). [Codex P3]
    let mut sel_sorted: Vec<u32> = selected.iter().copied().collect();
    sel_sorted.sort_unstable();
    for &eid in &sel_sorted {
        if eid as usize >= n_he { continue; }
        let he = mesh.he(HalfEdgeId(eid));
        if he.twin == NO_TWIN || he.face == NO_FACE { continue; }      // boundary → skip (v1)
        if mesh.he(he.twin).face == NO_FACE { continue; }              // twin boundary → skip
        // SLIT edge — the SAME face on both sides (a knife cut that ended mid-face: split_poly_slit
        // doubles the edge inside one face). The offset machinery needs two DISTINCT faces to build
        // rails against; a same-face edge sign-fixes both rails toward one centroid → folded garbage
        // (owner's "bevel pokes through near the cut end"). Blender parity: its dangling knife ends
        // are WIRE edges and bmesh bevel skips anything without two distinct faces. Skip → the rest
        // of the chain bevels normally and the slit is left untouched.
        if mesh.he(he.twin).face == he.face { continue; }              // slit (same-face) → skip
        if seen.contains(&eid) { continue; }
        seen.insert(eid);
        seen.insert(he.twin.0);
        beveled_he.insert(eid);
        beveled_he.insert(he.twin.0);
        primary_edges.push(eid);
    }
    if primary_edges.is_empty() {
        return;
    }

    // Original vertex positions (the algorithm reads these; install emits welded copies for rails).
    let orig_pos: Vec<Vec3> = mesh.vertices.iter().map(|v| v.position).collect();

    let mut weld = WeldMap::new();

    // Cache per-face centroid + normal once (sign-fixing the in-plane perpendicular).
    let mut face_centroid: HashMap<u32, Vec3> = HashMap::new();
    let mut face_normal: HashMap<u32, Vec3> = HashMap::new();
    let face_data = |fid: FaceId,
                     fc: &mut HashMap<u32, Vec3>,
                     fn_: &mut HashMap<u32, Vec3>| -> (Vec3, Vec3) {
        if let (Some(&c), Some(&n)) = (fc.get(&fid.0), fn_.get(&fid.0)) {
            return (c, n);
        }
        let verts = mesh.face_vertices(fid);
        let mut c = Vec3::ZERO;
        for &v in &verts { c += orig_pos[v.0 as usize]; }
        if !verts.is_empty() { c /= verts.len() as f32; }
        let n = mesh.compute_face_normal(fid);
        fc.insert(fid.0, c);
        fn_.insert(fid.0, n);
        (c, n)
    };

    // ---------------------------------------------------------------------------------------------
    // STEP 1 — Per directed beveled half-edge, build its in-plane offset rail (constant perpendicular).
    // Rail points are NOT welded yet (corner solving may relocate them); store the geometry first.
    // ---------------------------------------------------------------------------------------------
    let mut w = params.width.max(0.0);
    let width_req = w;
    // Undirected → primary half-edge id. Built HERE (not inside adjust_offsets below, which used to
    // own it) because the per-edge capacity map keys on the primary id too — one map, two readers.
    let und2prim: HashMap<u32, u32> = {
        let mut m: HashMap<u32, u32> = HashMap::with_capacity(primary_edges.len() * 2);
        for &e in &primary_edges {
            m.insert(e, e);
            m.insert(mesh.he(HalfEdgeId(e)).twin.0, e);
        }
        m
    };
    // PER-EDGE CAPACITY — the largest width THIS edge can take before its own neighbourhood
    // collapses. Same constraint values the global clamp has always computed; kept per edge instead
    // of collapsed into one minimum, which is what lets a variable width give a small feature its
    // own (smaller) bevel instead of dragging the whole selection down to it. The uniform path then
    // takes the global min of exactly this set, so `w` is bit-identical to before.
    let mut cap: HashMap<u32, f32> = HashMap::new();
    if params.clamp || var.is_some() {
        // CLAMP OVERLAP — Blender's `geometry_collide_offset` (clean-room port). Per beveled half-edge
        // B (vb→vc in face F), with A = F's previous edge (at vb) and C = F's next edge (at vc), and
        // k = 1 for a beveled edge / 0 for unbeveled:
        //   • COLLAPSE limit — the offset at which the advancing clones of A, B, C meet:
        //       len(B) / ( (ka + kb·cos θ₁)/sin θ₁ + (kc + kb·cos θ₂)/sin θ₂ )
        //   • SLIDE limit — when an adjacent edge is UNBEVELED, B's rail terminal-slides along it; walk
        //     the face over in-line (collinear) edges accumulating the EXTERIOR angle and sum
        //     len·sin(exterior) until the polygon actually turns.
        // Iterating BOTH halves of every beveled edge covers both incident faces (Blender iterates all
        // EdgeHalves per BevVert). This replaces the old blanket "half the shortest incident edge"
        // clamp, which cut the cylinder cap loop at 0.26·r where Blender allows 0.97·r (the user's
        // "this is the most I can bevel" report) and halved every chain/L bevel.
        let safe_div = |a: f32, b: f32| if b.abs() > 1e-9 { a / b } else { 0.0 };
        let ang3 = |a: Vec3, b: Vec3, c: Vec3| {
            (a - b).normalize_or_zero().dot((c - b).normalize_or_zero()).clamp(-1.0, 1.0).acos()
        };
        // Record one constraint against the edge it constrains (min-wins). A primary edge with NO
        // entry is UNCONSTRAINED — the caller below falls back to its own length.
        let mut note = |he_id: u32, v: f32| {
            if let Some(&p) = und2prim.get(&he_id) {
                let slot = cap.entry(p).or_insert(f32::INFINITY);
                if v < *slot { *slot = v; }
            }
        };
        for &he_id in &beveled_he {
            let he = mesh.he(HalfEdgeId(he_id));
            if he.face == NO_FACE { continue; }
            let hes = mesh.face_half_edges(he.face);
            let m = hes.len();
            if m < 3 { continue; }
            let j = match hes.iter().position(|h| h.0 == he_id) { Some(j) => j, None => continue };
            let prev_j = (j + m - 1) % m;
            let next_j = (j + 1) % m;
            let vb = mesh.he_source(HalfEdgeId(he_id)).0;
            let vc = he.vertex.0;
            let va = mesh.he_source(hes[prev_j]).0;
            let vd = mesh.he(hes[next_j]).vertex.0;
            let (pa, pb, pc, pd) = (orig_pos[va as usize], orig_pos[vb as usize],
                orig_pos[vc as usize], orig_pos[vd as usize]);
            let lb = (pc - pb).length();
            if lb < 1e-7 { continue; }
            let th1 = ang3(pa, pb, pc);
            let th2 = ang3(pb, pc, pd);
            let ka = if beveled_he.contains(&hes[prev_j].0) { 1.0f32 } else { 0.0 };
            let kc = if beveled_he.contains(&hes[next_j].0) { 1.0f32 } else { 0.0 };
            // A side imposes a COLLAPSE constraint only when its adjacent edge actually TURNS.
            // For an IN-LINE neighbour (θ→π, what an extrude leaves behind when it splits an edge
            // into collinear halves) or a degenerate fold (θ→0), sinθ→0 and this closed form blows
            // up: measured θ1=180° → denom 1.14e7 → limit 6.5e-8, so `w` was clamped to ZERO and
            // the bevel silently did nothing (owner 2026-08-02: beveling one segment of a collinear
            // run produced no geometry at all, while Blender gives a full watertight fillet).
            // Blender covers the straight case with the slide walk below, not with this formula.
            let collapse_term = |k: f32, th: f32| -> f32 {
                let s = th.sin();
                if s.abs() < 1e-4 { 0.0 } else { (k + th.cos()) / s }
            };
            let denom = collapse_term(ka, th1) + collapse_term(kc, th2);
            let _ = &safe_div;
            if std::env::var_os("AZRA_BEVEL_CLAMP_DEBUG").is_some() {
                eprintln!(
                    "[clamp] he={he_id} lb={lb:.4} th1={:.1}deg th2={:.1}deg ka={ka} kc={kc} denom={denom:.6} collapse={:.6}",
                    th1.to_degrees(), th2.to_degrees(),
                    if denom > 1e-6 { lb / denom } else { f32::INFINITY }
                );
            }
            if denom > 1e-6 { note(he_id, lb / denom); }
            // Slide walks (Blender: accumulate exterior angles across collinear runs).
            let mut slide_walk = |start: usize, start_th: f32, backward: bool| {
                let mut ext = std::f32::consts::PI - start_th;
                let mut idx = start;
                let mut slide = 0.0f32;
                for _ in 0..m {
                    let s = orig_pos[mesh.he_source(hes[idx]).0 as usize];
                    let d = orig_pos[mesh.he(hes[idx]).vertex.0 as usize];
                    slide += (d - s).length() * ext.sin();
                    if ext >= 1e-4 { break; }
                    if backward {
                        let p2 = (idx + m - 1) % m;
                        let a2 = orig_pos[mesh.he_source(hes[p2]).0 as usize];
                        ext += std::f32::consts::PI - ang3(a2, s, d);
                        idx = p2;
                    } else {
                        let n2 = (idx + 1) % m;
                        let d2 = orig_pos[mesh.he(hes[n2]).vertex.0 as usize];
                        ext += std::f32::consts::PI - ang3(s, d, d2);
                        idx = n2;
                    }
                }
                slide
            };
            if ka == 0.0 {
                let s = slide_walk(prev_j, th1, true);
                if s > 1e-6 { note(he_id, s); }
            }
            if kc == 0.0 {
                let s = slide_walk(next_j, th2, false);
                if s > 1e-6 { note(he_id, s); }
            }
        }
        // THE GLOBAL CLAMP — the GLOBAL min of the very same constraint set the loop above used to
        // fold into `limit`, so with `var == None` this is unchanged behaviour, one indirection later.
        // One minimum over the whole selection is the throttle: measured on a cube carrying a
        // 0.06-wide loop-cut band, a 0.2 request bevels the ENTIRE cube at 0.030. A policy replaces
        // it with each edge's own ceiling (further down), which is what lets a tight pair pin at its
        // meet point — where the `WeldMap` merges the two rails into one vertex at no extra cost.
        //
        // `var.is_none()` is a DEAD-WORK GUARD, not the mechanism: the policy path derives its
        // targets from `width_req`, never from `w`, so clamping `w` here could not reach it anyway
        // (sabotage-verified — removing this guard changes no gate). It stays because computing a
        // minimum nobody reads is waste, and because a future edit that starts reading `w` on the
        // policy path would otherwise silently reintroduce the global throttle.
        // A weights-only policy keeps the GLOBAL limit (`per_edge_limit == false`) — it scales the
        // width the user would otherwise have got, and must not also swap the clamp regime.
        if params.clamp && var.map_or(true, |v| !v.per_edge_limit) {
            let limit = cap.values().copied().fold(f32::INFINITY, f32::min);
            if limit.is_finite() { w = w.min(limit); }
        }
    }
    LAST_BEVEL_WIDTH_REQ.with(|c| c.set(width_req));
    LAST_BEVEL_WIDTH_EFF.with(|c| c.set(w));

    // ---------------------------------------------------------------------------------------------
    // VARIABLE WIDTH ([`BevelVarWidth`]) — per-edge TARGET width inside [width_min, params.width].
    // `None` skips this entirely; the uniform `w` (and its exact arithmetic) is untouched.
    //
    // The LARGE end is the REQUESTED width, not the globally clamped `w`: per-edge capacity is what
    // limits each edge here, so letting one small feature's global limit cap the band would rebuild
    // the very problem this exists to fix.
    // ---------------------------------------------------------------------------------------------
    let var_target: Option<HashMap<u32, f32>> = var.map(|v| {
        // `width_min` is the graded drivers' floor only. For `Uniform` the band collapses to the
        // requested width, so every edge targets it and ONLY the per-edge ceiling below can move it.
        // The band's large end is the REQUESTED width when this policy owns the limit; when it does
        // not (weights-only), it is the globally clamped `w` the untouched path would have used, so
        // the weight scales exactly what the user sees today.
        let top = if v.per_edge_limit { width_req } else { w };
        let lo = if v.driver.is_graded() { v.width_min.clamp(0.0, top) } else { top };
        // An edge with NO recorded constraint (both in-face neighbours beveled AND collinear, so
        // every collapse term vanished and no slide walk ran) is genuinely unbounded by its
        // neighbourhood. The two uses of capacity then diverge and must be kept apart:
        //
        //  • as a SIZE, it still needs a number on the SAME SCALE as everyone else's, or one such
        //    edge becomes `cap_max` and halves every other edge's share. Half the edge length is
        //    that scale — it is what the collapse formula yields at the ordinary 90° corner
        //    (len/denom, denom=2).
        //  • as a CEILING, there is nothing to clamp to. Leaving it unclamped is also what the
        //    uniform path does, since such an edge contributes no term to the global minimum.
        let half_len = |e: u32| -> f32 {
            let he = HalfEdgeId(e);
            0.5 * (orig_pos[mesh.he(he).vertex.0 as usize]
                - orig_pos[mesh.he_source(he).0 as usize])
                .length()
        };
        let size_of = |e: u32| -> f32 { cap.get(&e).copied().unwrap_or_else(|| half_len(e)) };
        let cap_max = primary_edges.iter().map(|&e| size_of(e)).fold(0.0f32, f32::max);
        // CORNER SPAN grades by the size of the DETAIL rather than of one segment (owner
        // 2026-08-11). Built only for that driver — it walks every beveled half-edge's face ring, and
        // the other drivers must stay bit-identical. An edge the span walk cannot measure (open on
        // one side, so there is no corner to span) falls back to `size_of`, keeping it on the same
        // scale as its neighbours instead of dropping it to zero and starving its bevel.
        let spans: HashMap<u32, f32> = if v.driver == BevelWidthDriver::CornerSpan {
            corner_span_sizes(mesh, &orig_pos, &beveled_he, &und2prim)
        } else {
            HashMap::new()
        };
        let span_of = |e: u32| -> f32 { spans.get(&e).copied().unwrap_or_else(|| size_of(e)) };
        let span_max = if v.driver == BevelWidthDriver::CornerSpan {
            primary_edges.iter().map(|&e| span_of(e)).fold(0.0f32, f32::max)
        } else {
            0.0
        };
        let mut out: HashMap<u32, f32> = HashMap::with_capacity(primary_edges.len());
        for &e in &primary_edges {
            let size_e = size_of(e);
            let t = match v.driver {
                // Not graded — `lo` is already the requested width, so t only has to be finite.
                BevelWidthDriver::Uniform => 1.0,
                // Roomiest edge in the selection gets t=1. A UNIFORM mesh has one capacity, so
                // every t is 1 and the result equals a plain bevel at `width` — turning the toggle
                // on can never surprise the user on regular geometry (gated by a test).
                BevelWidthDriver::AutoSize => {
                    if cap_max > 1e-9 { (size_e / cap_max).clamp(0.0, 1.0) } else { 1.0 }
                }
                // Roomiest CORNER gets t=1, same normalisation shape as AutoSize so the two graded
                // drivers stay comparable — only the measuring stick differs.
                BevelWidthDriver::CornerSpan => {
                    if span_max > 1e-9 { (span_of(e) / span_max).clamp(0.0, 1.0) } else { 1.0 }
                }
                // Legacy driver: authored weight used to BE the grading, with an unweighted edge
                // reading 0.0 (i.e. `width_min`). It is a multiplier now, applied below to every
                // driver, so this behaves as Uniform and the weight does the rest. Kept so
                // `.fl4` files saved with driver 2 keep loading.
                BevelWidthDriver::AuthoredWeight => 1.0,
            };
            let mut we = lo + (top - lo) * t;
            // AUTHORED WEIGHT = a percentage of whatever width this edge just got. Absent = 1.0,
            // so an unpainted mesh is untouched and painting only ever scales an edge DOWN.
            if !v.weights.is_empty() {
                let wt = v
                    .weights
                    .get(&e)
                    .or_else(|| v.weights.get(&mesh.he(HalfEdgeId(e)).twin.0))
                    .copied()
                    .unwrap_or(1.0)
                    .clamp(0.0, 1.0);
                we *= wt;
            }
            // §5 invariant 9, now per edge: capacity is the hard ceiling, so even a `width_min`
            // floor wider than this edge's room degrades to the room instead of inverting the face.
            if params.clamp {
                if let Some(&ceil) = cap.get(&e) { we = we.min(ceil); }
            }
            out.insert(e, we.max(0.0));
        }
        if params.clamp {
            let pair = opposing_pair_caps(mesh, &orig_pos, &beveled_he, &und2prim, &out);
            for (e, share) in pair {
                if let Some(we) = out.get_mut(&e) {
                    if share < *we { *we = share.max(0.0); }
                }
            }
        }
        out
    });
    if let Some(t) = &var_target {
        // The telemetry contract is "what the geometry got". With a band that is its widest edge,
        // so `last_bevel_width_was_clamped()` still reports true iff something was actually reduced.
        let mx = t.values().copied().fold(0.0f32, f32::max);
        LAST_BEVEL_WIDTH_EFF.with(|c| c.set(mx));
    }

    // ---------------------------------------------------------------------------------------------
    // ADJUST OFFSETS — Blender `adjust_offsets`, clean-room (BEVEL.md §11 2026-07-23(c)).
    // At every vertex where EXACTLY two beveled edges meet, each side's boundary point slides onto
    // a cross (fan) edge; sharing that slide point forces  w_b = w_a · sin(b,x)/sin(a,x)  (the
    // sinratio). Constraints link beveled edges into paths/cycles (an edge has 2 endpoints → node
    // degree ≤ 2). Per component: propagate the ratio product g_i from a root, then least-squares
    // the scale d minimizing Σ(d·g_i − w)² → d = w·Σg/Σg². Validated against the owner_ring oracle
    // (predicted 0.2324..0.2562, Blender 0.2331..0.2549, ~1%); WITHOUT this a band crossing curved
    // geometry keeps the raw uniform width and its seam kinks into straight chords (owner red-line).
    // Symmetric corners give ratio 1 → single/uniform selections are UNCHANGED (d = w exactly).
    let edge_w: HashMap<u32, f32> = {
        // `und2prim` is built once above (shared with the per-edge capacity map).
        // Collect constraints (a, b, r) with a,b = primary ids, w_b = w_a·r.
        let mut adj: HashMap<u32, Vec<(u32, f32)>> = HashMap::new();
        let mut verts: Vec<u32> = Vec::new();
        {
            let mut seen_v: HashSet<u32> = HashSet::new();
            for &e in &primary_edges {
                for vv in [mesh.he_source(HalfEdgeId(e)).0, mesh.he(HalfEdgeId(e)).vertex.0] {
                    if seen_v.insert(vv) { verts.push(vv); }
                }
            }
            verts.sort_unstable(); // determinism
        }
        for &v in &verts {
            let fan = mesh.vertex_fan(crate::half_edge::VertexId(v));
            let bev_fan: Vec<HalfEdgeId> = fan.iter().copied()
                .filter(|h| und2prim.contains_key(&h.0)).collect();
            if bev_fan.len() != 2 { continue; }
            let (pa, pb) = (und2prim[&bev_fan[0].0], und2prim[&bev_fan[1].0]);
            if pa == pb { continue; } // loop edge / degenerate
            let p = orig_pos[v as usize];
            let da = (orig_pos[mesh.he(bev_fan[0]).vertex.0 as usize] - p).normalize_or_zero();
            let db = (orig_pos[mesh.he(bev_fan[1]).vertex.0 as usize] - p).normalize_or_zero();
            let mut rs: Vec<f32> = Vec::new();
            for h in &fan {
                if und2prim.contains_key(&h.0) { continue; }
                let x = (orig_pos[mesh.he(*h).vertex.0 as usize] - p).normalize_or_zero();
                let sa = (1.0 - da.dot(x).powi(2)).max(0.0).sqrt();
                let sb = (1.0 - db.dot(x).powi(2)).max(0.0).sqrt();
                if sa > 0.05 && sb > 0.05 { rs.push(sb / sa); }
            }
            if rs.is_empty() { continue; }
            let r = rs.iter().sum::<f32>() / rs.len() as f32;
            adj.entry(pa).or_default().push((pb, r));
            adj.entry(pb).or_default().push((pa, 1.0 / r));
        }
        // Components: BFS propagate g, least-squares fit d per component.
        let mut out: HashMap<u32, f32> = HashMap::new();
        let mut visited: HashSet<u32> = HashSet::new();
        for &root in &primary_edges {
            if visited.contains(&root) { continue; }
            let mut comp: Vec<(u32, f32)> = vec![(root, 1.0f32)];
            visited.insert(root);
            let mut queue: Vec<(u32, f32)> = vec![(root, 1.0)];
            while let Some((cur, g)) = queue.pop() {
                if let Some(links) = adj.get(&cur) {
                    for &(nxt, r) in links {
                        if visited.insert(nxt) {
                            let gn = g * r;
                            comp.push((nxt, gn));
                            queue.push((nxt, gn));
                        }
                        // cycle-closing link intentionally dropped (product ≈ 1 on consistent
                        // cycles; residual error is spread by the least-squares fit).
                    }
                }
            }
            let sg: f32 = comp.iter().map(|(_, g)| g).sum();
            let sg2: f32 = comp.iter().map(|(_, g)| g * g).sum();
            // Least-squares scale for this component. The sinratio constraints stay AUTHORITATIVE
            // (they are what keeps a fillet's seam smooth across chained edges — BEVEL.md §11
            // 2026-07-23(c)/(d)); a variable width only moves the TARGET each edge is fitted to,
            // from one constant `w` to per-edge `w_i`:  d = Σ(g_i·w_i) / Σg_i².
            //
            // The uniform branch keeps the ORIGINAL expression verbatim rather than the equivalent
            // Σ(g_i·w): `w * sg / sg2` and `Σ(g_i·w) / sg2` differ in the last f32 bits, and every
            // pre-existing bevel gate is pinned to the former.
            let d = match &var_target {
                None => if sg2 > 1e-12 { w * sg / sg2 } else { w },
                Some(t) => {
                    let w_root = t.get(&root).copied().unwrap_or(w);
                    if sg2 > 1e-12 {
                        let dot_gw: f32 = comp
                            .iter()
                            .map(|(e, g)| g * t.get(e).copied().unwrap_or(w))
                            .sum();
                        dot_gw / sg2
                    } else {
                        w_root
                    }
                }
            };
            for (e, g) in comp {
                let we = (d * g).max(0.0);
                out.insert(e, we);
                out.insert(mesh.he(HalfEdgeId(e)).twin.0, we);
            }
        }
        out
    };
    let ew = |he_id: u32| -> f32 { edge_w.get(&he_id).copied().unwrap_or(w) };

    let mut rails: HashMap<u32, Rail> = HashMap::new();
    for &he_id in &beveled_he {
        let he = mesh.he(HalfEdgeId(he_id));
        let fid = he.face;
        if fid == NO_FACE { continue; }
        let src = mesh.he_source(HalfEdgeId(he_id)).0 as usize;
        let dst = he.vertex.0 as usize;
        let src_pos = orig_pos[src];
        let dst_pos = orig_pos[dst];
        let edge_vec = dst_pos - src_pos;
        let edge_len = edge_vec.length();
        if edge_len < 1e-7 { continue; }
        let edge_dir = edge_vec / edge_len;

        let (centroid, normal) = face_data(fid, &mut face_centroid, &mut face_normal);
        // In-plane perpendicular to the edge: n × e (lies in the face plane, ⟂ to the edge).
        let mut inward = normal.cross(edge_dir).normalize_or_zero();
        if inward.length_squared() < 1e-12 {
            // Degenerate (normal ∥ edge) — fall back to centroid direction projected in-plane.
            let to_c = centroid - 0.5 * (src_pos + dst_pos);
            inward = (to_c - normal * to_c.dot(normal)).normalize_or_zero();
        }
        // Sign: `normal × edge_dir` ALREADY points into the face — the interior of a simple polygon
        // is always LEFT of its directed (ring-order) boundary, convex or concave, and the Newell
        // normal is winding-consistent. The old "flip toward the centroid" check was WRONG on
        // CONCAVE faces (an L-shaped knife-cut region's centroid can sit on the far side of an
        // edge), flipping a correct rail inward → folded strips / coincident BoundVerts (the 90°
        // staircase non-manifold). No flip.

        rails.insert(he_id, Rail {
            src_idx: usize::MAX, // filled in STEP 2 after corner solving
            dst_idx: usize::MAX,
            src_base: src_pos,
            dir: inward,
            edge_dir,
            w: ew(he_id),
        });
    }

    // CRASH CLASS, FIXED AT THE SOURCE (owner 2026-08-05: two app crashes, "no entry found
    // for key", at rails[..] lookups — first at the corner solve, then at the ring build).
    // `beveled_he` is the SELECTION; `rails` holds only the edges that actually got a rail.
    // Eleven sites index `rails` while guarding on `beveled_he`, so ANY divergence is a
    // panic that takes the app down. With a FULL selection the two always agreed, which is
    // why this never appeared before — Smart Select made partial selections the normal case
    // and exposed it. Rather than guard eleven call sites (one was patched, the next crash
    // landed on another), make the predicate MEAN what the table contains: an edge without a
    // rail is not beveled. Every `beveled_he.contains(..)` guard now implies `rails` has the
    // key, so all eleven lookups are safe by construction.
    beveled_he.retain(|he| rails.contains_key(he));

    // ---------------------------------------------------------------------------------------------
    // STEP 2 — Per (beveled directed half-edge, endpoint) RAIL POINTS, welded.
    //
    // For half-edge `he` (src→dst on face F) the rail offsets BOTH endpoints inward by F's `dir`:
    //   rail_src = src + dir*w,  rail_dst = dst + dir*w  (constant perpendicular → parallel rail).
    // When two beveled edges meet at vertex V within the SAME face F (V = dst of `pe`, src of `ce`),
    // their two offset lines are coplanar in F and meet at ONE corner; both endpoints weld to that
    // intersection so the rails join cleanly (BEVEL_PLAN step 3). Otherwise the endpoint is the plain
    // `V + dir*w`. Welding by position guarantees a shared corner is ONE index across both rails/faces.
    // ---------------------------------------------------------------------------------------------
    let mut affected_faces: HashSet<u32> = HashSet::new();
    let mut beveled_vertices: HashSet<u32> = HashSet::new();
    for &he_id in &beveled_he {
        let he = mesh.he(HalfEdgeId(he_id));
        beveled_vertices.insert(mesh.he_source(HalfEdgeId(he_id)).0);
        beveled_vertices.insert(he.vertex.0);
    }
    for &v in &beveled_vertices {
        for &h in &mesh.vertex_fan(crate::half_edge::VertexId(v)) {
            let f = mesh.he(h).face;
            if f != NO_FACE { affected_faces.insert(f.0); }
        }
    }

    // ---------------------------------------------------------------------------------------------
    // STEP 2b — MITER build_boundary (Blender). Where two beveled edges meet at a RE-BEVELED (degree>3)
    // vertex the local rail solve cannot seal the join (rails on separate faces). Replace it with Blender's
    // build_boundary: a BoundVert per consecutive beveled pair (`build_boundary_flat`, incl. the general
    // drop-onto-face for ≥2 between-edges). `boundvert_override[(he,v)]` welds each incident rail to its
    // BoundVert (→ STEP 2/4); `corner_boundvert[ce]` maps every face-corner at the vertex to its sector
    // BoundVert (→ STEP 3 uniform corner). SCOPED to k==2 & degree>3 so working degree-3 corners are untouched.
    let mut miter_verts: HashSet<u32> = HashSet::new();
    let mut boundvert_override: HashMap<(u32, u32), Vec3> = HashMap::new();
    let mut corner_boundvert: HashMap<u32, Vec3> = HashMap::new();
    // ROUNDED SPAN CORNER (§11): a MIXED corner (2 < n_bev < degree, seg>1) with a run of 2+ CONSECUTIVE
    // unbeveled edges (a (false,false) sector). Blender's `build_boundary` places ONE BoundVert across that
    // span (offset_meet edges_between + drop-onto-face — the same `drop_onto_faces` we already use), so the
    // corner is a CLEAN count-n_bev BoundVert ring filled by the M_ADJ grid, NOT a flat clamp. We reuse
    // `build_boundary_flat` for the BoundVerts but — unlike a miter — do NOT delete-and-cap: the rails carry
    // real arcs and STEP 5 fills the grid. This set marks such corners so STEP 3 uses `corner_boundvert`
    // (like a miter) while STEP 5 still builds the rim + grid (unlike a miter, which STEP 5 skips).
    let mut span_round_verts: HashSet<u32> = HashSet::new();
    // N-WAY: a miter vertex with >2 BoundVerts (3+ beveled edges meeting at one degree>3 vertex) needs an
    // explicit face over its BoundVert ring — Blender's M_POLY vmesh. The 2-BoundVert case is a bare miter
    // edge (no cap). Value = the fan-ordered BoundVert ring positions (STEP 5 M_POLY cap consumes them).
    let mut miter_cap_rings: HashMap<u32, Vec<Vec3>> = HashMap::new();
    // ROUNDED 2-EDGE MITER (§3.7): the co-planarized weld arc bridging the two BoundVerts (per miter vertex
    // with exactly 2 BoundVerts and segments>1). Consumed in STEP 2.5 to build the shared arc both edges'
    // strips reference. N-way (>2 BoundVerts) is NOT rounded here (flat M_POLY cap).
    let mut rounded_2edge: HashSet<u32> = HashSet::new();  // miter verts that are exactly-2-BoundVert
    let mut miter_arc_pos: HashMap<u32, Vec<Vec3>> = HashMap::new();
    // ARC INNER MITER (§3.8/§3.8a) — populated ONLY when the caller asked for it. Where Sharp welds
    // the two edges to ONE shared chain (`miter_arc_pos`), Arc gives each edge its OWN side chain and
    // fills between them with a corner patch, so these three maps REPLACE `miter_arc_pos` at such a
    // vertex rather than sitting beside it.
    //   `miter_side[(v, primary_edge)]` — that edge's side chain, O → S (len n_seg+1).
    //   `miter_patch[v]`               — the patch levels, level i holding i+1 points (corners.rs).
    //   `miter_edges[v]`               — the two primary edges, in (d_a, d_b) order.
    let mut miter_side: HashMap<(u32, u32), Vec<Vec3>> = HashMap::new();
    let mut miter_patch: HashMap<u32, Vec<Vec<Vec3>>> = HashMap::new();
    let mut miter_edges: HashMap<u32, (u32, u32)> = HashMap::new();
    // Requested profile segments (before any clamp) — the miter arc rounds to this count.
    let req_seg = (params.segments.max(1)).min(32) as usize;
    let req_shape = params.shape.clamp(0.0, 1.0);
    {
        let mut mv: Vec<u32> = beveled_vertices.iter().copied().collect();
        mv.sort_unstable(); // deterministic weld order
        for v in mv {
            let fan = mesh.vertex_fan(crate::half_edge::VertexId(v));
            let degree = fan.len();
            let n_bev = fan.iter().filter(|h| beveled_he.contains(&h.0)).count();
            // STEP 2b fires ONLY for corners it can seal CORRECTLY:
            //   • n_bev==2       → the 2-edge miter (2 BoundVerts + 1 shared edge, NO cap; ROUNDED via the
            //     M_NONE weld arc `miter_arc_pos` when segments>1 — §3.7).
            //   • n_bev==degree  → a FULLY-beveled vertex: every sector between consecutive BoundVerts is a
            //     single beveled edge → a CLOSED BoundVert ring → an M_POLY cap that twins the strips.
            // A MIXED corner (2 < n_bev < degree) has a NON-beveled "wrap" sector; a cap across it would
            // orphan its radial edges (no strip there) → holes. So mixed corners are NOT routed through
            // STEP 2b — they fall back to the pre-existing path (guarded → rejected as a no-op). Full
            // mixed-corner support needs Blender's "on edge between" (eon) placement — future work.
            // [d706fda mixed-corner gate + 442ffb3 rounded-miter weld]
            // M_ADJ STEP-1 re-route: a ROUNDABLE clean apex (fully-beveled degree-4, segments>1) is NOT
            // collapsed to flat BoundVerts here — it flows through STEP 2/3/5 like a degree-3 corner so its
            // edges keep their arcs and STEP 5 domes the rounded boundary. seg=1 (flat) still uses the STEP-2b
            // M_POLY collapse. (Un-clamp allows it below; if STEP 2/3/5 can't seal it the guard rejects it.)
            let round_apex = n_bev == degree && degree >= 4 && req_seg > 1;
            // DEGREE-2 chain BEND (knife-cut bend vertex: exactly two edges, both beveled). Blender
            // deletes V and miters the strip through 2 BoundVerts — NO cap (oracle
            // knife_chain_seg{1,3}_ref.json: 2 offset-line intersection points, seg>1 = the M_NONE weld
            // arc between them). Routing it through STEP 2b gets exactly that machinery; the old
            // fall-through hit CAP-NEEDED's unconditional k>=2 cap, whose fan-walk around a valence-2
            // vertex emitted DUPLICATE fan faces (non-manifold "directed edge used by 2 faces").
            let chain_bend = degree == 2 && n_bev == 2;
            // ROUNDED k=2 AT DEGREE-3 (owner live-console clamp class 2026-07-23 — the canonical
            // "bevel a face's edge loop" corner). Oracle k2d3_adj2/loop4: Blender's corner here is
            // the SAME M_NONE weld as the 2-edge miter (V deleted, one BoundVert on the shared
            // face + one slid onto the unbeveled edge, ONE shared profile arc both strips reuse)
            // — no grid. Route through STEP 2b ONLY when rounding (req_seg>1); the seg=1 flat
            // path keeps the proven legacy k≥2 n-gon patch (edge_loop_corner_patch gate).
            let miter_2edge_d3 = n_bev == 2 && degree == 3 && req_seg > 1;
            if ((n_bev == 2 || n_bev == degree) && degree > 3 && !round_apex)
                || chain_bend
                || miter_2edge_d3
            {
                let (bvs, r_ov, c_bv, m_arc) =
                    build_boundary_flat(mesh, v, &beveled_he, &orig_pos, &ew, req_seg, req_shape);
                if bvs.len() >= 2 {
                    miter_verts.insert(v);
                    boundvert_override.extend(r_ov);
                    corner_boundvert.extend(c_bv);
                    // Cap only the fully-beveled closed ring (n_bev==degree → bvs.len()==n_bev>2). The
                    // 2-edge miter (bvs.len()==2) never caps.
                    if bvs.len() > 2 { miter_cap_rings.insert(v, bvs); }
                    else {
                        rounded_2edge.insert(v);
                        if let Some(arc) = m_arc { miter_arc_pos.insert(v, arc); }
                    }
                }
            } else if n_bev >= 3 && n_bev < degree {
                // MIXED CORNER RING (§11 2026-08-15): 3+ beveled edges and at least one unbeveled one.
                // EVERY such corner takes its BoundVerts from `build_boundary`, never from per-rail
                // terminal slides. Two rails flanking the SAME unbeveled edge, each slid independently
                // by `w/sin`, land on two DIFFERENT points unless the corner happens to be symmetric —
                // so the rim is a 10-point ring instead of a 9-point chained arc ring, the M_ADJ grid
                // declines it, and the corner degrades to a fan (or, with the cap suppressed, to the
                // end-of-pipeline seal's single flat n-gon: the owner's broken round, §11).
                // `build_boundary_flat` places ONE BoundVert per consecutive beveled pair — the
                // across-SPAN drop for a (false,false) sector, the projected meet for a lone in-between
                // edge — and both bracketing rails weld to it (r_ov), so their arcs SHARE it.
                // This is `span_round_verts`' original job, just no longer restricted to spans; the flat
                // (seg=1) profile needs the same single BoundVert, so it is no longer gated on req_seg.
                let (bvs, r_ov, c_bv, _) =
                    build_boundary_flat(mesh, v, &beveled_he, &orig_pos, &ew, req_seg, req_shape);
                if bvs.len() == n_bev {
                    span_round_verts.insert(v);
                    boundvert_override.extend(r_ov);
                    corner_boundvert.extend(c_bv);
                }
            }
        }
    }

    // Resolve a rail endpoint position for half-edge `he_id` at corner vertex `v`. If the ADJACENT
    // half-edge in the same face (sharing `v`) is also beveled, return the line-intersection corner;
    // else the plain perpendicular offset. Free function (NOT a closure capturing `rails`) so the read
    // phase's immutable borrow ends before the write phase mutates `rails` (read → decide → write).
    fn rail_endpoint_pos(
        mesh: &HalfEdgeMesh, rails: &HashMap<u32, Rail>, beveled_he: &HashSet<u32>, orig_pos: &[Vec3],
        n_face: Vec3, _w: f32, he_id: u32, v: u32,
    ) -> Vec3 {
        let he = mesh.he(HalfEdgeId(he_id));
        let r = &rails[&he_id];
        let w = r.w; // per-edge adjusted width (adjust_offsets)
        let v_pos = orig_pos[v as usize];
        let (adj, this_is_src) = if mesh.he_source(HalfEdgeId(he_id)).0 == v {
            (he.prev.0, true)   // v is src → neighbour is prev (its dst is v)
        } else {
            (he.next.0, false)  // v is dst → neighbour is next (its src is v)
        };
        // CRASH GUARD (owner 2026-08-05, "app crash trying to extrude after a boolean" —
        // panic at this line, "no entry found for key"). `beveled_he` is the SELECTION, but
        // `rails` only holds edges that actually got a rail built, and the two can disagree:
        // a partial selection (Smart Select excluding boolean support edges) can leave a
        // selected neighbour with no rail. Indexing then panicked and took the app down.
        // Treat a rail-less neighbour as NOT beveled — that is the terminal-edge slide path
        // below, which is exactly what an unselected neighbour gets. Fail closed: correct
        // geometry for the case we can compute, never a crash.
        if beveled_he.contains(&adj) && rails.contains_key(&adj) {
            let ar = &rails[&adj];
            if this_is_src {
                solve_corner_lines2(v_pos, n_face, ar.dir, ar.edge_dir, ar.w, r.dir, r.edge_dir, r.w)
            } else {
                solve_corner_lines2(v_pos, n_face, r.dir, r.edge_dir, r.w, ar.dir, ar.edge_dir, ar.w)
            }
        } else {
            // Adjacent in-face edge is NOT beveled — the terminal-edge case. SLIDE the rail ONTO that edge
            // (Blender loop slide, distance w/sin(angle)) so it stays ON the surface, rather than the plain
            // perpendicular `v+dir*w` which LIFTS it when the adjacent face is tilted (a bevel landing on a
            // prior chamfer). Orthogonal corner: sin(90°)=1 → slide == plain (golden single-edge unchanged).
            // CLAMP the slide to the adjacent edge length (Blender `slide_dist`): w/sin is unbounded as the
            // corner sharpens and the width clamp only bounds the PERPENDICULAR width, so an acute corner
            // would slide PAST the far vertex and self-overlap. Falls back to plain offset if degenerate.
            let a = HalfEdgeId(adj);
            let adj_far = if mesh.he_source(a).0 == v {
                orig_pos[mesh.he(a).vertex.0 as usize]
            } else {
                orig_pos[mesh.he_source(a).0 as usize]
            };
            let bev_dir = if this_is_src { r.edge_dir } else { -r.edge_dir };
            let adj_vec = adj_far - v_pos;
            let adj_len = adj_vec.length();
            let adj_dir = adj_vec.normalize_or_zero();
            let ang = bev_dir.dot(adj_dir).clamp(-1.0, 1.0).acos();
            let s = ang.sin();
            const GOOD: f32 = 0.01;
            if adj_len > 1e-6 && ang > GOOD && (ang - std::f32::consts::PI).abs() > GOOD && s > 1e-4 {
                let d = (w / s).min((adj_len - 50.0 * 1e-6).max(0.0));
                v_pos + adj_dir * d
            } else {
                v_pos + r.dir * w
            }
        }
    }

    // READ PHASE: compute every rail's src/dst endpoint position (needs the face normal, cached here).
    // Sort the keys so the WRITE PHASE welds points in a STABLE order → deterministic vertex IDs. [Codex P3]
    let mut rail_keys: Vec<u32> = rails.keys().cloned().collect();
    rail_keys.sort_unstable();
    let mut rail_endpoints: Vec<(u32, Vec3, Vec3)> = Vec::with_capacity(rail_keys.len());
    for he_id in &rail_keys {
        let he = mesh.he(HalfEdgeId(*he_id));
        let fid = he.face;
        let (_c, n_face) = face_data(fid, &mut face_centroid, &mut face_normal);
        let src = mesh.he_source(HalfEdgeId(*he_id)).0;
        let dst = he.vertex.0;
        // A miter vertex's BoundVert (STEP 2b) overrides the local rail solve.
        let sp = boundvert_override.get(&(*he_id, src)).copied()
            .unwrap_or_else(|| rail_endpoint_pos(mesh, &rails, &beveled_he, &orig_pos, n_face, w, *he_id, src));
        let dp = boundvert_override.get(&(*he_id, dst)).copied()
            .unwrap_or_else(|| rail_endpoint_pos(mesh, &rails, &beveled_he, &orig_pos, n_face, w, *he_id, dst));
        rail_endpoints.push((*he_id, sp, dp));
    }
    // WRITE PHASE: weld + assign indices (immutable `rails` borrow above is now dropped).
    for (he_id, sp, dp) in rail_endpoints {
        let si = weld.push(sp);
        let di = weld.push(dp);
        let r = rails.get_mut(&he_id).unwrap();
        r.src_idx = si;
        r.dst_idx = di;
        let _ = r.src_base; // reserved for v2 profile interpolation
    }
    let _primary_set: HashSet<u32> = primary_edges.iter().copied().collect();

    // ---------------------------------------------------------------------------------------------
    // STEP 2c — ROUNDED CHAIN WELD (degree-3, k==2). An edge-CHAIN / cap-LOOP vertex (two beveled edges
    // + one unbeveled at a degree-3 vertex — the cylinder cap loop, an L-selection) is Blender's 2-BoundVert
    // weld case: STEP 2 already produced both BoundVerts (the shared-face rail MEET via solve_corner_lines
    // + the terminal SLIDE onto the unbeveled edge), and the corner is sealed by the profile arc bridging
    // them, SHARED by both edges' strips (M_NONE — no cap, no grid). The oracle `chain2_cube_ref.json`
    // decodes the arc as the ELLIPSE of Blender's unit-square profile map: c = A + B − V,
    // p(θ) = c + cosθ·(A−c) + sinθ·(B−c), θ∈[0,π/2] — vertex-exact ((0.85,−0.85,0.95981) at seg 3).
    // NOTE this is NOT `circular_arc` (slerp + radius-lerp): the chain's two legs are UNEQUAL (meet ≠
    // slide distance), where only the ellipse form matches Blender. Store the chain in `miter_arc_pos`
    // (the same M_NONE infrastructure as the degree>3 rounded miter): `build_at` returns THE SAME welded
    // chain for both edges → the strips twin along it → watertight by construction.
    // Guard: only fires when the 4 rail endpoints at the vertex reduce to exactly 2 welded positions
    // (skewed corners where the two slides disagree keep the flat clamp — Blender's
    // `offset_on_edge_between` compromise slide is future work).
    if req_seg > 1 {
        let mut cv: Vec<u32> = beveled_vertices.iter().copied().collect();
        cv.sort_unstable(); // deterministic
        'chain: for v in cv {
            let fan = mesh.vertex_fan(crate::half_edge::VertexId(v));
            if fan.len() != 3 { continue; }
            // The (up to 2) primary beveled edges incident at v.
            let mut prim_at: Vec<u32> = Vec::new();
            for &h in &fan {
                let tw = mesh.he(h).twin;
                let hb = beveled_he.contains(&h.0)
                    || (tw != NO_TWIN && beveled_he.contains(&tw.0));
                if hb {
                    let p = if _primary_set.contains(&h.0) { h.0 } else { tw.0 };
                    if !prim_at.contains(&p) { prim_at.push(p); }
                }
            }
            if prim_at.len() != 2 { continue; }
            // The 4 rail endpoints at v (2 per edge, L + R). L runs src→dst; R is the twin (reversed).
            let mut pos: Vec<Vec3> = Vec::with_capacity(4);
            for &pe in &prim_at {
                let twin_id = mesh.he(HalfEdgeId(pe)).twin.0;
                let (l, r) = match (rails.get(&pe), rails.get(&twin_id)) {
                    (Some(l), Some(r)) => (l, r),
                    _ => continue 'chain,
                };
                let (li, ri) = if mesh.he_source(HalfEdgeId(pe)).0 == v {
                    (l.src_idx, r.dst_idx)
                } else {
                    (l.dst_idx, r.src_idx)
                };
                pos.push(weld.positions[li]);
                pos.push(weld.positions[ri]);
            }
            let mut groups: Vec<(Vec3, usize)> = Vec::new();
            for p in &pos {
                match groups.iter_mut().find(|g| (g.0 - *p).length() < 1e-4) {
                    Some(g) => g.1 += 1,
                    None => groups.push((*p, 1)),
                }
            }
            if groups.len() != 2 || groups[0].1 != 2 || groups[1].1 != 2 { continue; }
            let (a, b) = (groups[0].0, groups[1].0);
            let apex = orig_pos[v as usize];
            let c = a + b - apex;
            let (ua, ub) = (a - c, b - c);
            if ua.length_squared() < 1e-10 || ub.length_squared() < 1e-10 { continue; }
            let chain: Vec<Vec3> = (0..=req_seg).map(|i| {
                let t = i as f32 / req_seg as f32;
                let ang = t * std::f32::consts::FRAC_PI_2;
                let ell = c + ua * ang.cos() + ub * ang.sin();
                let flat = a.lerp(b, t);
                if req_shape <= 0.5 { flat.lerp(ell, req_shape * 2.0) }
                else { ell + (ell - flat) * ((req_shape - 0.5) * 2.0) }
            }).collect();
            rounded_2edge.insert(v); // un-clamps + skips the corner cap (M_NONE — the chain IS the seal)
            // ARC INNER MITER (§3.8). The sharp chain runs between the shared-face rail MEET and the
            // SLIDE onto the unbeveled edge; the arc replaces the SLIDE end (that is the "inner meet
            // point M" the oracle spreads — §3.8's S0/S1 sit at M's height, displaced along the two
            // beveled edges). Identify M geometrically rather than by group order: it is the endpoint
            // whose offset from V runs along the single UNBEVELED fan edge.
            let arc_built = 'arc: {
                let Some(spread) = miter_arc else { break 'arc false };
                let unbev: Vec<HalfEdgeId> = fan.iter().copied().filter(|h| {
                    let tw = mesh.he(*h).twin;
                    !(beveled_he.contains(&h.0) || (tw != NO_TWIN && beveled_he.contains(&tw.0)))
                }).collect();
                if unbev.len() != 1 { break 'arc false; }
                let u_dir = (orig_pos[mesh.he(unbev[0]).vertex.0 as usize] - apex).normalize_or_zero();
                if u_dir.length_squared() < 0.5 { break 'arc false; }
                let along = |p: Vec3| (p - apex).normalize_or_zero().dot(u_dir);
                // Fail CLOSED: if neither endpoint is convincingly the slide, keep Sharp rather than
                // spreading the wrong end (a skewed corner where the two slides disagree).
                let (o, m_pt, chain_om) = if along(b) > 0.99 && along(a) < 0.9 {
                    (a, b, chain.clone())
                } else if along(a) > 0.99 && along(b) < 0.9 {
                    (b, a, chain.iter().rev().copied().collect::<Vec<Vec3>>())
                } else {
                    break 'arc false;
                };
                let _ = o;
                // Beveled-edge directions at V, pointing AWAY from it — the axes S0/S1 spread along.
                let dir_of = |pe: u32| -> Vec3 {
                    let h = HalfEdgeId(pe);
                    let far = if mesh.he_source(h).0 == v { mesh.he(h).vertex.0 } else { mesh.he_source(h).0 };
                    (orig_pos[far as usize] - apex).normalize_or_zero()
                };
                let (d_a, d_b) = (dir_of(prim_at[0]), dir_of(prim_at[1]));
                if d_a.length_squared() < 0.5 || d_b.length_squared() < 0.5 { break 'arc false; }
                // The spread cannot exceed what the corner has room for: each end slides back along a
                // beveled edge, so half that edge's length is the hard ceiling (beyond it the arc
                // crosses the edge's far corner). Fail closed by CLAMPING, never by folding.
                let room = |pe: u32| -> f32 {
                    let h = HalfEdgeId(pe);
                    let far = if mesh.he_source(h).0 == v { mesh.he(h).vertex.0 } else { mesh.he_source(h).0 };
                    (orig_pos[far as usize] - apex).length() * 0.5
                };
                let spread = spread.min(room(prim_at[0])).min(room(prim_at[1]));
                if spread <= 1e-6 { break 'arc false; }
                let levels = inner_miter_patch(&chain_om, d_a, d_b, spread);
                if levels.len() != req_seg + 1 { break 'arc false; }
                let _ = m_pt;
                miter_side.insert((v, prim_at[0]), levels.iter().map(|l| l[0]).collect());
                miter_side.insert((v, prim_at[1]), levels.iter().enumerate().map(|(i, l)| l[i]).collect());
                miter_edges.insert(v, (prim_at[0], prim_at[1]));
                miter_patch.insert(v, levels);
                true
            };
            if !arc_built {
                miter_arc_pos.insert(v, chain);
            }
        }
    }

    // ---------------------------------------------------------------------------------------------
    // STEP 2.5 — SEGMENTS (round bevel): per beveled edge, build the arc of rail lines from face L to
    // face R. segments==1 → the flat chamfer (arc = [L, R], one strip). segments>1 → n+1 arc points at
    // each endpoint (quadratic Bézier, control pulled toward the ORIGINAL edge vertex by `shape`), n
    // strips. The endpoint / corner faces insert the SAME welded arc points so the round stays watertight.
    // ---------------------------------------------------------------------------------------------
    // Rounded corners where ≥2 beveled edges MEET are handled per case: a ROUNDED 2-EDGE MITER (exactly two
    // beveled edges at a degree>3 vertex) IS supported — the M_NONE weld arc (§3.7, STEP 2b `miter_arc_pos`)
    // bridges the two BoundVerts, watertight. Every OTHER shared corner (edge-loop k=2 degree-3, N-way ≥3
    // beveled edges) still forces a FLAT chamfer (n_seg=1): those need Blender's `M_ADJ` "Grid Fill" grid
    // which the single center-fan cap can't produce without self-intersecting (verified: removing the clamp
    // self-intersects 10/30 bevel-on-beveled corners). Isolated/non-adjacent rounded edges keep full n_seg.
    let mut n_seg = (params.segments.max(1)).min(32) as usize;
    if n_seg > 1 {
        // ROUNDED shared corner (M_ADJ v1). We ROUND clean shared corners (cube corner, pyramid /
        // cylinder-cap apex) via the STEP-5 dome, but must NOT round a BEVEL-ON-BEVELED corner — beveling an
        // edge that lands on an already-beveled (degree>3) vertex gives a k=1 PARTIAL corner whose rim is
        // NON-convex; rounding its strips + corner self-intersects (verified 8/36). Signature = a beveled
        // vertex that is k=1 (exactly one beveled edge) AT degree>3 (a bevel-created vertex). When present,
        // keep the FLAT clamp for the whole bevel (safe, watertight); the full Catmull-Clark M_ADJ grid
        // (§3.7) is the future fix. Clean primitives (corners degree≤3, or k≥2 at degree>3) round.
        let unroundable_blocking = beveled_vertices.iter().any(|&v| {
            let fan = mesh.vertex_fan(crate::half_edge::VertexId(v));
            let degree = fan.len();
            if rounded_2edge.contains(&v) { return false; } // handled by the M_NONE weld
            if span_round_verts.contains(&v) { return false; } // across-span BoundVert → clean chain (STEP 2b)
            let mut legs = 0;
            for &h in &fan {
                if beveled_he.contains(&h.0) { legs += 1; }
                let tw = mesh.he(h).twin;
                if tw != NO_TWIN && beveled_he.contains(&tw.0) { legs += 1; }
            }
            let k = legs / 2;
            // A blocking (k≥2) corner ROUNDS when its rim is a clean n×seg arc chain filled by the M_ADJ
            // grid (STEP 5): fully-beveled corners (k==degree), and MIXED corners with k≥3 whose unbeveled
            // edges are all ISOLATED (no two consecutive in the fan — a (false,false) sector would insert
            // the original V into the rim and break the chain). The oracle-verified k=3-of-4 case
            // (`mixed_k3d4_ref.json`): the flanking rails terminal-slide onto each lone unbeveled edge (the
            // on-edge BoundVert) and the corner fills as a 3-sector grid. Everything else stays FLAT:
            // k=2 that STEP 2b could not weld, and mixed corners with a (false,false) sector — the
            // remaining M_ADJ-with-vertex-profiles gap (§3.7).
            if k < 2 || k == degree { return false; }
            let bev: Vec<bool> = fan.iter().map(|h| {
                let tw = mesh.he(*h).twin;
                beveled_he.contains(&h.0) || (tw != NO_TWIN && beveled_he.contains(&tw.0))
            }).collect();
            let two_consec_unbev = (0..degree).any(|i| !bev[i] && !bev[(i + 1) % degree]);
            let blocking = !(k >= 3 && !two_consec_unbev);
            if blocking {
                // Diagnostic (owner 2026-07-23 "round bevel stays flat"): name the corner class
                // that forces the global flat clamp so the console shows WHY, not just a flat result.
                eprintln!(
                    "[BEVEL] FLAT-CLAMP by v{v}: degree={degree} k={k} bev={:?} pos={:?} \
                     (k=2-unweldable or (false,false)-sector corner — BEVEL.md §9.4 remainder)",
                    bev, orig_pos.get(v as usize)
                );
            }
            blocking
        });
        if unroundable_blocking { n_seg = 1; }
    }
    // Publish effective segment count for the app (HUD / toast when round is forced flat).
    LAST_BEVEL_EFFECTIVE_SEG.with(|c| c.set(n_seg as u32));
    LAST_BEVEL_FORCED_FLAT.with(|c| c.set(n_seg < req_seg));
    // If a BLOCKING shared corner forced the flat clamp (or clamp-overlap otherwise lowered the count), the
    // strips run at `n_seg` segments but the STEP-2b miter chain was built at `req_seg`. Drop any chain whose
    // segment count no longer matches so `build_at` falls back to the (flat) collapsed-rail build — keeping
    // the strip/arc lengths consistent. The pure rounded-2-edge selection keeps its chain (n_seg == req_seg).
    if n_seg != req_seg {
        miter_arc_pos.clear();
        // Same reason for the ARC patch: its side chains and levels were built at `req_seg`, so a
        // clamped strip would bridge arcs of two different lengths. Dropping them falls back to the
        // flat collapsed-rail build, i.e. Sharp — the fail-closed direction.
        miter_side.clear();
        miter_patch.clear();
        miter_edges.clear();
    }
    let shape = params.shape.clamp(0.0, 1.0); // 0 = flat chamfer, 0.5 = circular fillet, 1 = convex bulge
    // Beveled-edge count per vertex. A vertex with ≥3 beveled edges is a fully-surrounded corner whose rails
    // the corner-solve trims INWARD off the vertex plane — there (and only there) the arc apex must be
    // re-projected into the fillet cross-section (else it bulges out of plane, breaking edge parallelism).
    // A simple (1-edge) or loop-slid miter vertex is NOT trimmed and must keep the un-projected apex.
    let mut bev_count: HashMap<u32, usize> = HashMap::new();
    for &eid in &primary_edges {
        *bev_count.entry(mesh.he_source(HalfEdgeId(eid)).0).or_default() += 1;
        *bev_count.entry(mesh.he(HalfEdgeId(eid)).vertex.0).or_default() += 1;
    }
    // arc_at[(primary_edge_id, vertex_id)] = welded indices, face-L → face-R (len n_seg+1).
    let mut arc_at: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    let mut edge_arcs: HashMap<u32, (Vec<usize>, Vec<usize>)> = HashMap::new(); // per edge: (src_arc, dst_arc)
    for &eid in &primary_edges {
        let twin_id = mesh.he(HalfEdgeId(eid)).twin.0;
        let (li, ri) = match (rails.get(&eid), rails.get(&twin_id)) {
            (Some(l), Some(r)) => ((l.src_idx, l.dst_idx), (r.src_idx, r.dst_idx)),
            _ => continue,
        };
        let src_v = mesh.he_source(HalfEdgeId(eid)).0;
        let dst_v = mesh.he(HalfEdgeId(eid)).vertex.0;
        // Edge direction (original geometry). The bevel fillet is an arc swept along the edge, so its
        // cross-section lies in the plane PERPENDICULAR to this direction.
        let e_dir = (orig_pos[dst_v as usize] - orig_pos[src_v as usize]).normalize_or_zero();
        // L-rail runs src→dst; the R-rail is the TWIN (dst→src) so its src_idx sits at the original DST
        // and its dst_idx at the original SRC.
        let (src_l, dst_l) = (weld.positions[li.0], weld.positions[li.1]);
        let (src_r, dst_r) = (weld.positions[ri.1], weld.positions[ri.0]);
        let build = |a_l: Vec3, a_r: Vec3, orig: Vec3, weld: &mut WeldMap| -> Vec<usize> {
            // Profile blend: shape 0 = FLAT chord, 0.5 = CIRCULAR (apex at the original vertex → true
            // circular arc, matches Blender's superellipse 0.5 exactly), 1 = CONVEX (circular pushed out
            // 2×). Blending flat↔circular↔convex is correct across the whole range, unlike apex-scaling the
            // circle (which distorted the sub-0.5 range into a mis-centred circle instead of flattening).
            (0..=n_seg).map(|i| {
                let t = i as f32 / n_seg as f32;
                let circ = circular_arc(a_l, a_r, orig, t);
                let flat = a_l.lerp(a_r, t);
                let p = if shape <= 0.5 {
                    flat.lerp(circ, shape * 2.0)
                } else {
                    circ + (circ - flat) * ((shape - 0.5) * 2.0)
                };
                weld.push(p)
            }).collect()
        };
        // ROUNDED 2-EDGE MITER (§3.7): at a rounded miter vertex the two rails collapsed to ONE BoundVert
        // (boundvert_override), so `build` would emit a degenerate arc. Instead weld the SHARED miter chain
        // (`miter_arc_pos[v]`, from BoundVert A→B) in THIS edge's L→R orientation. Both beveled edges' strips
        // reference the SAME welded positions (WeldMap dedups) → the M_NONE weld is watertight by construction.
        let build_at = |v: u32, a_l: Vec3, a_r: Vec3, weld: &mut WeldMap| -> Vec<usize> {
            // ARC INNER MITER (§3.8): unlike Sharp — where both edges weld to ONE shared chain below —
            // each edge takes its OWN side chain, which is the whole point of the mode. Oriented the
            // same way (chain runs O→S; reverse if this edge's L rail is the S end).
            if let Some(side) = miter_side.get(&(v, eid)) {
                let d0 = (side[0] - a_l).length_squared();
                let dn = (side[side.len() - 1] - a_l).length_squared();
                let seq: Vec<Vec3> =
                    if d0 <= dn { side.clone() } else { side.iter().rev().copied().collect() };
                let _ = a_r;
                return seq.iter().map(|&p| weld.push(p)).collect();
            }
            if miter_arc_pos.contains_key(&v) {
                let chain = &miter_arc_pos[&v];
                // Orient the chain so chain[0] matches this edge's L rail (a_l). The chain runs A→B; if a_l
                // is closer to B, reverse. (a_l/a_r are the two BoundVert positions here.)
                let d0 = (chain[0] - a_l).length_squared();
                let dn = (chain[chain.len() - 1] - a_l).length_squared();
                let iter: Vec<Vec3> = if d0 <= dn { chain.clone() } else { chain.iter().rev().copied().collect() };
                let _ = a_r;
                return iter.iter().map(|&p| weld.push(p)).collect();
            }
            // At a fully-surrounded corner (≥3 beveled edges) the corner-solve trims the rails inward off the
            // vertex's edge-plane; bulging the arc toward the un-projected 3D vertex then pulls the arc-mid OUT
            // of the fillet cross-section, breaking edge parallelism (mine (0.746,0.873,0.873) vs Blender
            // (0.7,0.912,0.912) on the ±1 cube). Re-project the apex into the cross-section plane (⊥ edge,
            // through the rail midpoint) THERE only. Simple/loop-slid vertices keep the un-projected apex.
            let orig = orig_pos[v as usize];
            let mut apex = if bev_count.get(&v).copied().unwrap_or(0) >= 3 {
                let anchor = (a_l + a_r) * 0.5;
                orig - e_dir * e_dir.dot(orig - anchor)
            } else {
                orig
            };
            // Owner 2026-08-08: edge ending at a 3-spoke vertex could fillet CONVEX on one end
            // and CONCAVE on the other. Force the arc to bulge toward the original edge vertex
            // (concave reentrant / convex outer both share that geometric rule; the bug was the
            // mid-arc landing on the wrong side of the rail chord).
            apex = apex_bulge_toward_edge(a_l, a_r, apex);
            build(a_l, a_r, apex, weld)
        };
        let src_arc = build_at(src_v, src_l, src_r, &mut weld);
        let mut dst_arc = build_at(dst_v, dst_l, dst_r, &mut weld);
        // ROW ALIGNMENT for miter-chain ends. STEP 4 bridges src_arc[i] ↔ dst_arc[i]; rows must run
        // PARALLEL to the edge. A miter chain (M_NONE weld) is stored A→B in fan order, which is
        // face-AGNOSTIC — at an S-turn (e.g. a 90° staircase knife cut, where the inner side swaps
        // between consecutive bends) the a_l-distance orientation picks the same direction for both
        // edges, so one strip consumes the chain REVERSED → crossed quads → the same arc segment
        // bounded by 4 faces (non-manifold "directed edge used by 2 faces"). Align by the direct
        // criterion instead: if pairing endpoints straight (0↔0, n↔n) is longer than crossed
        // (0↔n, n↔0), the dst arc is reversed relative to src — flip it. Only chain-derived arcs may
        // be flipped (a rail-built arc's L→R order is a real face convention used by STEP 3).
        if (miter_arc_pos.contains_key(&dst_v) || miter_side.contains_key(&(dst_v, eid)))
            && src_arc.len() == dst_arc.len() && src_arc.len() >= 2
        {
            let p = |i: usize| weld.positions[i];
            let n = src_arc.len() - 1;
            let straight = (p(src_arc[0]) - p(dst_arc[0])).length() + (p(src_arc[n]) - p(dst_arc[n])).length();
            let crossed = (p(src_arc[0]) - p(dst_arc[n])).length() + (p(src_arc[n]) - p(dst_arc[0])).length();
            if crossed < straight {
                dst_arc.reverse();
            }
        }
        if std::env::var_os("AZRA_BEVEL_DBG").is_some() {
            let f = |a: &Vec<usize>| a.iter().map(|&i| {
                let q = weld.positions[i];
                format!("{}({:.2},{:.2})", i, q.x, q.y)
            }).collect::<Vec<_>>().join(" ");
            eprintln!("[BEVELDBG] eid={} src_v={} dst_v={} src=[{}] dst=[{}]", eid, src_v, dst_v, f(&src_arc), f(&dst_arc));
        }
        arc_at.insert((eid, src_v), src_arc.clone());
        arc_at.insert((eid, dst_v), dst_arc.clone());
        edge_arcs.insert(eid, (src_arc, dst_arc));
    }

    // ---------------------------------------------------------------------------------------------
    // ARC INNER MITER — weld the corner patch, and record how the two WALL faces absorb its top arc.
    // Everything here is welded through `WeldMap`, so the patch's two side chains land on the SAME
    // indices the strips already built from `miter_side` (§5 invariant 2) and twin by construction.
    // Faces are held aside and appended with the other corner fills (STEP 5b) because `faces_out`
    // does not exist yet at this point in the pipeline.
    // ---------------------------------------------------------------------------------------------
    let mut patch_faces: Vec<Vec<usize>> = Vec::new();
    let mut arc_wall_runs: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    {
        let mut pv: Vec<u32> = miter_patch.keys().copied().collect();
        pv.sort_unstable(); // determinism (§5 invariant 7)
        for v in pv {
            let levels = &miter_patch[&v];
            let ns = levels.len() - 1;
            let (ea, eb) = miter_edges[&v];
            let lv: Vec<Vec<usize>> = levels
                .iter()
                .map(|l| l.iter().map(|&p| weld.push(p)).collect::<Vec<usize>>())
                .collect();
            // ORIENTATION, topologically (never by normal guess). STEP 4 winds dst→src→src→dst, so a
            // strip traverses its arc i→i+1 at the arc's SRC vertex and i+1→i at the DST vertex. The
            // patch must run the OTHER way along the shared side or the two faces don't twin.
            let side_a0 = lv[0][0];
            let Some(arc_a) = arc_at.get(&(ea, v)) else { continue };
            if arc_a.len() != ns + 1 { continue; }
            let same = arc_a[0] == side_a0;
            let v_is_src = mesh.he_source(HalfEdgeId(ea)).0 == v;
            let strip_ascends = if same { v_is_src } else { !v_is_src };
            // The face pattern below walks level i forward then level i+1 back — i.e. DESCENDING on
            // side A — so it only needs flipping when the strip descends there too.
            let flip = !strip_ascends;
            let mut emit = |mut f: Vec<usize>| {
                if flip { f.reverse(); }
                patch_faces.push(f);
            };
            for i in 0..ns {
                let (p, q) = (&lv[i], &lv[i + 1]);
                // Level i+1 has ONE more point than level i. Spend that extra in the MIDDLE (one
                // triangle) rather than at an end, so a symmetric corner fills symmetrically.
                let m = i / 2;
                let (mut jp, mut jq) = (0usize, 0usize);
                while jp < i || jq < i + 1 {
                    if jp == m && jq == m {
                        emit(vec![p[jp], q[jq + 1], q[jq]]);
                        jq += 1;
                    } else {
                        emit(vec![p[jp], p[jp + 1], q[jq + 1], q[jq]]);
                        jp += 1;
                        jq += 1;
                    }
                }
            }
            // WALL ABSORPTION (§3.8a). Top arc index 0 = S0 (the `ea` side), ns = S1 (`eb`). Each wall
            // takes the run from its own edge's end to the split, S-end FIRST; STEP 3 reverses it when
            // the ring leaves the corner along that edge.
            let top = &lv[ns];
            let split = inner_miter_wall_split(ns);
            arc_wall_runs.insert((v, ea), top[..=split].to_vec());
            arc_wall_runs.insert((v, eb), top[split..].iter().rev().copied().collect());
        }
    }

    // ---------------------------------------------------------------------------------------------
    // CAP-NEEDED classification (the load-bearing coupling). A rounded corner CAP is emitted at V iff the
    // arc-interior / hole opened there is NOT already sealed by an opposite face:
    //   • k≥2 (two+ beveled edges meet) → the offset always opens a hole → cap.
    //   • k==1 at a degree>3 vertex with n_seg≥2 → the single edge's rounded arc opens a hole with NO
    //     degree-3 opposite face to bound it (on a clean cube the third face bounds the whole arc; at a
    //     bevel-created degree>3 vertex there is none) → cap. This is the bevel-on-beveled failure.
    // Re-adding V in the `(false,false)` re-trace and emitting the cap are COUPLED: V's radial cap edges
    // (V→arc_end / arc_end→V) only get a twin when a cap exists. So `(false,false)` re-adds V ⟺ cap_needed,
    // else it keeps the degree-3 same-edge whole-arc path (which the passing round single-edge test needs).
    // ---------------------------------------------------------------------------------------------
    let mut cap_needed: HashSet<u32> = HashSet::new();
    let mut partial_center: HashMap<u32, Vec3> = HashMap::new();
    // Partial corner → one slide point per NON-beveled incident edge, keyed by the undirected
    // edge (min of the half-edge pair). Blender's general partial-corner cap (§9.5); supersedes
    // `partial_center` at degree ≥ 5, where the "unique back edge" the centre relied on no
    // longer exists. Degree-3/4 corners keep the centre (oracle-verified there).
    let mut partial_slides: HashMap<u32, Vec<(u32, Vec3)>> = HashMap::new();
    for &v in &beveled_vertices {
        // BOUNDARY (open-rim) vertex: the "corner hole" is not a hole — it opens onto the mesh
        // boundary, and Blender leaves it open (rim_vertex_seg{1,3}.json: nv=10/6, nv=14/8, no cap,
        // no center). Capping here builds a lid over the rim whose fan folds onto the coplanar rim
        // faces (owner BEVEL EDGE CASE.fl4: folded_coplanar_pairs=2). The arc endpoints already lie
        // ON the rim, so leaving them uncapped extends the boundary loop without spawning a new one.
        if mesh.is_boundary_vertex(crate::half_edge::VertexId(v)) { continue; }
        if miter_verts.contains(&v) { continue; } // handled by build_boundary (STEP 2b): 2 BoundVerts, no cap
        // Rounded CHAIN weld vertex (STEP 2c): sealed by the shared M_NONE chain both strips twin along —
        // a cap here would double-cover the (already-closed) corner.
        if rounded_2edge.contains(&v) { continue; }
        let fan = mesh.vertex_fan(crate::half_edge::VertexId(v));
        let degree = fan.len();
        let mut legs = 0;
        for &h in &fan {
            if beveled_he.contains(&h.0) { legs += 1; }
            let tw = mesh.he(h).twin;
            if tw != NO_TWIN && beveled_he.contains(&tw.0) { legs += 1; }
        }
        let k = legs / 2;
        // A cap seals the corner hole. k≥2 always opens one. For k==1 the hole exists when no single incident
        // face can absorb the whole arc — the degree-3 `same_edge` seal. That seal exists on a clean convex
        // corner (the opposite face borders the beveled edge on BOTH its sides) but NOT on a degree>3
        // (bevel-created / natural high-valence) vertex, where the un-beveled "back" faces are separated from
        // the beveled edge by other fan edges → nothing bounds the chamfer arc → hole. Detect the seal
        // directly rather than by degree. Cap iff k==1 with no same_edge seal AND either the profile is
        // ROUNDED (n_seg≥2 opens interior arc points to twin at ANY degree) OR the far-endpoint is degree>3
        // (the FLAT k=1 partial corner — the octahedron-base hole: at seg=1 the single chamfer still opens
        // because no back face reaches it). The flat degree-3 corner keeps has_same_edge_seal=true → no cap.
        let mut has_same_edge_seal = false;
        if k == 1 {
            let prim_of = |h: u32| if _primary_set.contains(&h) { h } else { mesh.he(HalfEdgeId(h)).twin.0 };
            for &ce in &fan {
                if mesh.he(ce).face == NO_FACE { continue; }
                let pe = mesh.he(ce).prev;
                if beveled_he.contains(&ce.0) || beveled_he.contains(&pe.0) { continue; }
                let pe_bev = { let t = mesh.he(pe).twin;
                    if t != NO_TWIN { let x = mesh.he(t).prev.0; if beveled_he.contains(&x) { Some(x) } else { None } } else { None } };
                let ce_bev = { let t = mesh.he(ce).twin;
                    if t != NO_TWIN { let x = mesh.he(t).next.0; if beveled_he.contains(&x) { Some(x) } else { None } } else { None } };
                if let (Some(pa), Some(cb)) = (pe_bev, ce_bev) {
                    if prim_of(pa) == prim_of(cb) { has_same_edge_seal = true; break; }
                }
            }
        }
        // Concave crease + low-degree endpoint: NEVER emit a partial-corner spoke fan.
        // Owner 2026-08-08: one end of a multi-seg reentrant edge stayed a smooth concave
        // fillet while the other (3–4 spoke vertex, missed same_edge seal) built a radial
        // triangle fan that pinches to a sharp point (reads as convex). Re-trace absorbs the
        // whole arc like the sealed degree-3 L-step path. Degree ≥5 keeps §9.5 slides/hexagon
        // (owner_step oracle). Convex cube / octahedron corners are unaffected (not concave).
        let concave_edge_k1 = k == 1 && fan.iter().any(|&h| {
            let hid = h.0;
            if beveled_he.contains(&hid) {
                return edge_is_concave(mesh, HalfEdgeId(hid));
            }
            let t = mesh.he(h).twin;
            t != NO_TWIN && beveled_he.contains(&t.0) && edge_is_concave(mesh, t)
        });
        if concave_edge_k1 && degree <= 4 {
            continue;
        }
        if k >= 2 || (k == 1 && !has_same_edge_seal && (n_seg >= 2 || degree > 3)) {
            cap_needed.insert(v);
            if k == 1 {
                // PARTIAL corner (single beveled edge into a degree>3 / re-beveled vertex). Blender DELETES the
                // sharp corner V and places ONE recessed CENTER vertex, shared between the cap fan and the
                // neighbouring re-traced faces.
                //
                // Correct placement (Blender `offset_on_edge_between`, oracle-verified): the merged center
                // slides off V by exactly the offset `w` along the "back" fan edge — the ONE non-beveled edge
                // whose BOTH incident faces are un-beveled (opposite the bevel in the fan). At the degree-4
                // octahedron base this is X↔(−Y): center = V + w·normalize(far − V) lands EXACTLY on the
                // Blender oracle (0.8232,−0.1768,0). If no unique back edge exists (the arc-centroid fallback),
                // recess the arc centroid toward V by 0.3 — the prior heuristic, still watertight for the
                // round degree>3 bevel-on-beveled corners the geometric slide doesn't uniquely define.
                let vpos = orig_pos[v as usize];
                let k1w = fan.iter().find_map(|h| {
                    if beveled_he.contains(&h.0) { return Some(ew(h.0)); }
                    let t = mesh.he(*h).twin;
                    if t != NO_TWIN && beveled_he.contains(&t.0) { Some(ew(t.0)) } else { None }
                }).unwrap_or(w);
                // §9.5 (owner oracle, degree-5): Blender does NOT collapse a partial corner to one
                // point — it slides a vertex off V by the offset along EVERY non-beveled incident
                // edge and caps with a single n-gon over [arc…, slides]. `partial_back_edge_slide`
                // is precisely that rule's degree-4 case (exactly ONE remaining edge), which is why
                // the octahedron matched; at degree-5 two edges remain, the "unique back edge" is
                // undefined, and the old code fell through to the centroid heuristic → 1 vertex
                // where Blender has 3, a fan of slivers instead of the hexagon (owner: "the mess").
                // Emit the general slide set whenever ≥1 edge remains; degree-3 (none remain) keeps
                // the centre fan, whose rim is the bare arc and would otherwise be degenerate.
                // The two FLANKING fan edges (immediately before/after the beveled one) already
                // carry the arc's endpoints — the rails terminal-slide onto them. Only the edges
                // BEYOND them get a fresh slide point: degree−3 of them, which is 1 at degree-4
                // (exactly `partial_back_edge_slide`, so the octahedron oracle is untouched) and 2
                // at the owner's degree-5 hexagon. Degree-3 leaves none → centre fan below.
                // SCOPE — degree ≥ 5 only. Degree-4 keeps `partial_back_edge_slide` (octahedron
                // oracle). Degree-5+ uses multi-slide hexagon (§9.5). The spoke-fan look on
                // multi-seg partial corners is fixed in STEP 5 (n-gon, not triangle fan).
                if degree >= 5 {
                    let mut slides: Vec<(u32, Vec3)> = Vec::new();
                    for &h in &fan {
                        let t = mesh.he(h).twin;
                        if beveled_he.contains(&h.0) || (t != NO_TWIN && beveled_he.contains(&t.0)) {
                            continue;
                        }
                        let far = orig_pos[mesh.he(h).vertex.0 as usize];
                        let dir = (far - vpos).normalize_or_zero();
                        if dir.length_squared() < 0.5 { continue; }
                        let key = if t == NO_TWIN { h.0 } else { h.0.min(t.0) };
                        slides.push((key, vpos + dir * k1w));
                    }
                    if !slides.is_empty() {
                        partial_slides.insert(v, slides);
                        continue;
                    }
                }
                let back = partial_back_edge_slide(mesh, &beveled_he, &orig_pos, v, &fan, k1w);
                if let Some(c) = back {
                    partial_center.insert(v, c);
                } else {
                    let mut arc_c = Vec3::ZERO;
                    let mut cnt = 0usize;
                    let mut seen_e: HashSet<u32> = HashSet::new();
                    for &h in &fan {
                        for hid in [h.0, mesh.he(h).twin.0] {
                            if hid == u32::MAX || !beveled_he.contains(&hid) { continue; }
                            let prim = if _primary_set.contains(&hid) { hid } else { mesh.he(HalfEdgeId(hid)).twin.0 };
                            if !seen_e.insert(prim) { continue; }
                            if let Some(a) = arc_at.get(&(prim, v)) {
                                for &ix in a { arc_c += weld.positions[ix]; cnt += 1; }
                            }
                        }
                    }
                    if cnt > 0 {
                        arc_c /= cnt as f32;
                        partial_center.insert(v, arc_c + (vpos - arc_c) * 0.3);
                    }
                }
            }
        }
    }
    // Materialize each partial-corner center once (shared index across the fan cap + re-traced faces).
    // Sorted iteration → deterministic center vertex IDs. [Codex P3]
    let mut center_idx: HashMap<u32, usize> = HashMap::new();
    let mut pc_keys: Vec<u32> = partial_center.keys().cloned().collect();
    pc_keys.sort_unstable();
    for v in pc_keys { center_idx.insert(v, weld.push(partial_center[&v])); }
    // Materialize the per-edge slide points (§9.5). Sorted → deterministic ids. A slide point on a
    // FLANKING edge coincides with the beveled edge's arc endpoint; `WeldMap::push` quantizes, so
    // the two collapse to one index and the cap/strip/face rings stay watertight by construction.
    let mut slide_idx: HashMap<(u32, u32), usize> = HashMap::new();
    let mut ps_keys: Vec<u32> = partial_slides.keys().cloned().collect();
    ps_keys.sort_unstable();
    for v in ps_keys {
        let mut sl = partial_slides[&v].clone();
        sl.sort_by_key(|(k, _)| *k);
        for (k, p) in sl {
            let i = weld.push(p);
            slide_idx.insert((v, k), i);
        }
    }
    // Undirected key for an incident edge (matches how `partial_slides` was keyed).
    let ekey_of = |h: HalfEdgeId, mesh: &HalfEdgeMesh| -> u32 {
        let t = mesh.he(h).twin;
        if t == NO_TWIN { h.0 } else { h.0.min(t.0) }
    };

    // ---------------------------------------------------------------------------------------------
    // STEP 3 — Rebuild every face. Untouched faces copy verbatim. Affected faces re-trace, replacing a
    // beveled corner with the rail point(s) of the beveled half-edge(s) at that corner (the four-case
    // substitution that keeps the mesh watertight): for a corner at V between incoming `pe` and
    // outgoing `ce`, emit pe's dst-rail and/or ce's src-rail; an un-beveled-but-adjacent face (both
    // false) at a beveled V inserts that V's rail points so the face still closes against the chamfer.
    // ---------------------------------------------------------------------------------------------
    let mut orig_vert_idx: HashMap<u32, usize> = HashMap::new();
    let weld_orig = |v: u32, weld: &mut WeldMap, orig_vert_idx: &mut HashMap<u32, usize>| -> usize {
        if let Some(&i) = orig_vert_idx.get(&v) { return i; }
        let i = weld.push(orig_pos[v as usize]);
        orig_vert_idx.insert(v, i);
        i
    };

    let mut faces_out: Vec<Vec<usize>> = Vec::new();
    // Parallel material_id per faces_out entry — restored after install (else materials vanish).
    let mut faces_mats: Vec<u32> = Vec::new();

    // Face iteration order MUST stay 0..n for full-rebuild (multi-stage oracle HE ids).
    // Local path (large meshes) skips clean faces in the soup — install only uses dirty rings.
    let use_local = mesh.face_count() >= 8_192;
    let n_faces = mesh.faces.len();
    for i in 0..n_faces {
        let fid = FaceId(i as u32);
        let verts = mesh.face_vertices(fid);
        if verts.len() < 3 { continue; } // free / degenerate slot
        let face_mat = face_material_id(mesh, fid);
        if !affected_faces.contains(&(i as u32)) {
            if use_local {
                continue; // clean faces stay live on the mesh
            }
            let ring: Vec<usize> = verts
                .iter()
                .map(|v| weld_orig(v.0, &mut weld, &mut orig_vert_idx))
                .collect();
            faces_out.push(ring);
            faces_mats.push(face_mat);
            continue;
        }

        let hes = mesh.face_half_edges(fid);
        let m = hes.len();
        let mut ring: Vec<usize> = Vec::new();
        for j in 0..m {
            let ce = hes[j];                                  // outgoing from corner V (src = V)
            let pe = hes[(j + m - 1) % m];                    // incoming to V (dst = V)
            let v = mesh.he_source(ce).0;
            if !beveled_vertices.contains(&v) {
                ring.push(weld_orig(v, &mut weld, &mut orig_vert_idx));
                continue;
            }
            if miter_verts.contains(&v) {
                // Miter vertex (STEP 2b): V is DELETED, replaced by its BoundVert ring. This face's corner
                // is its sector's BoundVert — `corner_boundvert[ce]` = ce.leftv. One lookup replaces the
                // 4-case substitution and closes the "between" faces uniformly.
                match corner_boundvert.get(&ce.0).copied() {
                    Some(p) => ring.push(weld.push(p)),
                    None => ring.push(weld_orig(v, &mut weld, &mut orig_vert_idx)),
                }
                continue;
            }
            let pe_b = beveled_he.contains(&pe.0);
            let ce_b = beveled_he.contains(&ce.0);
            // ROUNDED SPAN CORNER (STEP 2b): a FULLY-unbeveled face at V collapses its V-corner to the
            // across-span BoundVert (the same point the bracketing beveled arcs share). Beveled faces at V
            // keep their normal rail/arc corner (fall through to the match below).
            if span_round_verts.contains(&v) && !pe_b && !ce_b {
                match corner_boundvert.get(&ce.0).copied() {
                    Some(p) => ring.push(weld.push(p)),
                    None => ring.push(weld_orig(v, &mut weld, &mut orig_vert_idx)),
                }
                continue;
            }
            // ARC INNER MITER (§3.8a): the two WALL faces at an arc vertex ABSORB the top arc — the
            // unbeveled edge's foot moves off the sharp meet onto the split vertex, and each wall
            // takes the run between its own beveled edge's arc end and that split. Measured
            // (miter_inner_wall_split.py), not inferred. The shared (true,true) face keeps its single
            // corner: §3.8's outer end is NOT spread.
            if miter_edges.contains_key(&v) && (pe_b != ce_b) {
                let bev_he = if ce_b { ce.0 } else { pe.0 };
                let prim = if _primary_set.contains(&bev_he) {
                    bev_he
                } else {
                    mesh.he(HalfEdgeId(bev_he)).twin.0
                };
                if let Some(run) = arc_wall_runs.get(&(v, prim)) {
                    // The ring leaves this corner along `ce`. If `ce` is the beveled edge, the run
                    // must END at that edge's arc (S first in storage → reverse); if we ARRIVED along
                    // it (`pe` beveled), the run starts there.
                    if ce_b {
                        ring.extend(run.iter().rev().copied());
                    } else {
                        ring.extend(run.iter().copied());
                    }
                    continue;
                }
            }
            match (pe_b, ce_b) {
                (true, true) => {
                    // Both edges beveled in this face: they share ONE corner point (welded equal).
                    ring.push(rails[&ce.0].src_idx);
                }
                (false, true) => {
                    ring.push(rails[&ce.0].src_idx);
                }
                (true, false) => {
                    ring.push(rails[&pe.0].dst_idx);
                }
                (false, false) => {
                    // V is beveled via an edge on ANOTHER face. Cut THIS face's corner only on the
                    // side(s) that actually border a beveled edge's face: the pe-side neighbour placed
                    // its rail at the shared edge (its beveled edge = pe.twin.prev), likewise the ce-side
                    // (ce.twin.next). Inserting only those adjacent rails — NOT every beveled edge's whole
                    // arc — is correct at ANY vertex degree. (The old whole-arc insertion was only right
                    // for a degree-3 cube corner; at a degree>3 bevel-created vertex it over-inserted and
                    // left an unsealed hole — the "bevel on already-beveled geometry" tear.)
                    let pe_bev = { let t = mesh.he(pe).twin;
                        if t != NO_TWIN { let x = mesh.he(t).prev.0; if beveled_he.contains(&x) { Some(x) } else { None } } else { None } };
                    let ce_bev = { let t = mesh.he(ce).twin;
                        if t != NO_TWIN { let x = mesh.he(t).next.0; if beveled_he.contains(&x) { Some(x) } else { None } } else { None } };
                    let prim_of = |h: u32| if _primary_set.contains(&h) { h } else { mesh.he(HalfEdgeId(h)).twin.0 };
                    let same_edge = match (pe_bev, ce_bev) {
                        (Some(pa), Some(cb)) => prim_of(pa) == prim_of(cb),
                        _ => false,
                    };
                    if cap_needed.contains(&v) {
                        // A rounded corner CAP will seal V here → insert only the per-side arc ENDPOINTS
                        // (G1: endpoints only — the strip + cap own the arc interior, never this flat face)
                        // and RE-ADD V between them. V supplies the twin for the cap's radial edges
                        // (V→arc_end / arc_end→V); without it those cap edges are orphaned (spec §7.2).
                        // Tokens are ALREADY in ring order: this face is traced pe→V→ce, so the pe-side
                        // rail point (where the beveled edge slid ALONG this face's incoming edge) comes
                        // first, then V, then the ce-side rail point. Do NOT re-sort — the old angle-sort
                        // flipped [pe_end, V] → [V, pe_end] on non-planar degree-4 corners, so the shared
                        // edge disagreed with the E-bordering face → the pronged non-manifold boundary.
                        // §9.5 partial corner with per-edge slides: V is deleted and this face meets the
                        // corner through ITS OWN two edges' points — the beveled side's arc endpoint, or
                        // the slide point of an un-beveled side. (The old single shared centre pulled
                        // every neighbouring face to one vertex; that is the sliver fan.)
                        let has_slides = partial_slides.contains_key(&v);
                        if has_slides {
                            if let Some(pa) = pe_bev {
                                ring.push(rails[&pa].dst_idx);
                            } else if let Some(&si) = slide_idx.get(&(v, ekey_of(pe, mesh))) {
                                ring.push(si);
                            }
                            if let Some(cb) = ce_bev {
                                ring.push(rails[&cb].src_idx);
                            } else if let Some(&si) = slide_idx.get(&(v, ekey_of(ce, mesh))) {
                                ring.push(si);
                            }
                        } else {
                            if let Some(pa) = pe_bev { ring.push(rails[&pa].dst_idx); }
                            // PARTIAL corner: connect to the shared recessed CENTER (Blender deletes V). The
                            // center is shared with the cap fan → the C↔arc-endpoint edges twin. A non-partial
                            // (k≥2) corner keeps V (its own n-gon cap seals it).
                            if let Some(&ci) = center_idx.get(&v) { ring.push(ci); }
                            else { ring.push(weld_orig(v, &mut weld, &mut orig_vert_idx)); }
                            if let Some(cb) = ce_bev { ring.push(rails[&cb].src_idx); }
                        }
                    } else if same_edge {
                        // No cap here (degree-3 corner): the opposite face bounds the WHOLE arc, whose
                        // segments directly twin the strip's arc segments. Insert the arc from the pe-side
                        // rail point to the ce-side — DETERMINISTICALLY: arc index 0 lies on the beveled
                        // edge's `face` (fl); if THIS face's incoming edge `pe` borders fl, arc[0] is the
                        // pe-side endpoint (insert as-is), else reverse. (The old angle-sort flipped on a
                        // non-planar re-beveled corner → the arc ran the SAME way as the strip → doubled.)
                        let prim = prim_of(pe_bev.unwrap());
                        let fl = mesh.he(HalfEdgeId(prim)).face;
                        let pe_twin = mesh.he(pe).twin;
                        let pe_face = if pe_twin != NO_TWIN { mesh.he(pe_twin).face } else { NO_FACE };
                        if let Some(a) = arc_at.get(&(prim, v)) {
                            let mut ins = a.clone();
                            if pe_face != fl { ins.reverse(); }
                            for ix in ins { ring.push(ix); }
                        }
                    } else {
                        // Distinct edges / only one adjacent side: insert just that side's rail endpoint(s).
                        let mut got = false;
                        if let Some(pa) = pe_bev { ring.push(rails[&pa].dst_idx); got = true; }
                        if let Some(cb) = ce_bev { ring.push(rails[&cb].src_idx); got = true; }
                        if !got { ring.push(weld_orig(v, &mut weld, &mut orig_vert_idx)); }
                    }
                }
            }
        }
        dedup_ring(&mut ring);
        if ring.len() >= 3 {
            faces_out.push(ring);
            faces_mats.push(face_mat);
        }
    }

    // AUTO_POLYGROUPS: STEP 3 finished — re-traced originals end here; new geometry starts next.
    let new_face_start = faces_out.len() as u32;

    // ---------------------------------------------------------------------------------------------
    // STEP 4 — Strip quad per beveled edge bridging the two rails. WINDING is critical: `from_polygons`
    // twins half-edges by REVERSE direction, so the strip must traverse each rail OPPOSITE to the face
    // it bridges, or both copies of the rail edge run the SAME way, fail to twin, and become boundary
    // edges (the non-watertight bug). The primary face traverses its rail l.src→l.dst; the strip must
    // run l.dst→l.src. The twin face traverses its rail r.src→r.dst; the strip must run r.dst→r.src.
    // Loop: l.dst → l.src → r.dst → r.src (the two cross edges are the chamfer's src/dst ends).
    // ---------------------------------------------------------------------------------------------
    for &eid in &primary_edges {
        let (src_arc, dst_arc) = match edge_arcs.get(&eid) { Some(a) => a, None => continue };
        // Inherit material from an adjacent face (prefer non-zero so multi-side edges keep the look).
        let fl = mesh.he(HalfEdgeId(eid)).face;
        let fr = {
            let t = mesh.he(HalfEdgeId(eid)).twin;
            if t != NO_TWIN { mesh.he(t).face } else { NO_FACE }
        };
        let ml = face_material_id(mesh, fl);
        let mr = face_material_id(mesh, fr);
        let strip_mat = if ml > 0 { ml } else { mr };
        // n_seg strips between consecutive arc rail lines. Winding matches the flat case at i=0/i=n_seg
        // (dst_arc[0]/src_arc[0] = L rail, ..[n_seg] = R rail) so face-L/face-R twins stay watertight and
        // interior rails twin between adjacent strips.
        for i in 0..n_seg {
            let mut q = vec![dst_arc[i], src_arc[i], src_arc[i + 1], dst_arc[i + 1]];
            dedup_ring(&mut q);
            if q.len() >= 3 {
                faces_out.push(q);
                faces_mats.push(strip_mat);
            }
        }
    }

    // ---------------------------------------------------------------------------------------------
    // STEP 5 — Rounded corner CAP (vertex-mesh) at each `cap_needed` vertex. Build the rim as ONE loop by
    // walking the vertex fan in cyclic order (NEVER angle-sort — that scrambles the arc-interior points of
    // a rounded corner). Each beveled fan edge contributes its V-arc, chained onto the loop by SHARED
    // welded endpoint so the arc's storage direction is irrelevant; each un-beveled `(false,false)` sector
    // contributes the persisting V (which twins the re-trace's re-added V). Orient the loop ONCE against
    // the vertex normal (Newell-of-loop fallback for vn≈0 saddles), then emit: a single n-gon if convex,
    // else a centroid fan (every fan triangle is simple → a curved rim can't self-intersect the cap). The
    // cap sits interior to the rim; strips + re-traced faces sit exterior of the SAME segments → opposite
    // traversal → watertight twin by construction (the PHASE-8 debug audit asserts zero residual boundary).
    // ---------------------------------------------------------------------------------------------
    // STRIP / CAP BOUNDARY. Everything emitted so far in the new range is STRIP geometry --
    // the band running ALONG the beveled edge; everything after is CORNER FILL at the ends.
    // Smart Select needs the distinction: the strip's long rims are tangent to the round and
    // must be skipped, while the corner-fill's boundary edges are real corners the user still
    // wants beveled (owner 2026-08-04: "keep side edge from bevel, only the round segment",
    // then "those 2 too" for the long rims).
    LAST_BEVEL_CAP_START.with(|c| c.set(faces_out.len() as u32));
    let mut cap_verts: Vec<u32> = beveled_vertices.iter().copied().collect(); // sorted → stable face IDs [Codex P3]
    cap_verts.sort_unstable();
    for &v in &cap_verts {
        if !cap_needed.contains(&v) { continue; }
        // NOTE (§11 2026-08-15) — there used to be a "skip the cap when every unbeveled sector is
        // tangent-continuous" guard here, added 2026-08-04 because the cap folded against the
        // re-traced faces at a corner where a FLAT boolean seam lands. It was a bandaid over a
        // BROKEN RIM: the two rails flanking that seam were each slid onto it independently and
        // landed on two different points, so the rim was not a chained arc ring. Suppressing the
        // cap merely handed the corner to the end-of-pipeline closed-input seal, which plastered
        // ONE FLAT n-gon over a rim that Blender fills with the M_ADJ dome — the owner's "the edge
        // from the boolean creates 4 edges that break the rounded cap". Both rails now weld to one
        // BoundVert (STEP 2b mixed-corner ring), the rim is clean, and the cap is the corner's only
        // filler — nothing left to fold against. Do NOT reintroduce the skip: an unbeveled edge
        // being flat says nothing about whether the corner needs filling; Blender builds the
        // identical dome with or without it (`flat_unbeveled_edge_must_not_break_the_rounded_corner`).
        let cap_mat = vertex_bevel_material(mesh, v);
        let vn0 = mesh.compute_vertex_normal(crate::half_edge::VertexId(v));

        // Rim in fan order; arcs chained by shared endpoint; `(false,false)` sectors drop in V. V is
        // materialized LAZILY (only when a genuine (false,false) sector needs it) — the same V the STEP-3
        // re-trace re-added — so k≥2 corners that never touch a (false,false) sector don't create a
        // stranded, unused V vertex (that inflated edge_loop's vert count).
        let mut loop_pts: Vec<usize> = Vec::new();
        let mut bv_set: HashSet<usize> = HashSet::new(); // arc-endpoint (BoundVert) welded indices
        let fan = mesh.vertex_fan(crate::half_edge::VertexId(v));
        for &ce in &fan {
            let pe = mesh.he(ce).prev;
            let ce_b = beveled_he.contains(&ce.0);
            let pe_b = beveled_he.contains(&pe.0);
            if ce_b {
                let prim = if _primary_set.contains(&ce.0) { ce.0 } else { mesh.he(ce).twin.0 };
                if let Some(a) = arc_at.get(&(prim, v)) {
                    if let (Some(&a0), Some(&a1)) = (a.first(), a.last()) {
                        bv_set.insert(a0);
                        bv_set.insert(a1);
                    }
                    append_chained(a, &mut loop_pts);
                }
            } else if pe_b {
                // Transition sector: `ce` is un-beveled but the previous edge `pe` IS beveled, so this
                // sector's V-endpoint is already the arc endpoint we appended when we processed that
                // beveled edge. Add NOTHING (re-appending the whole arc double-owns it — the flat-k≥2
                // corner-cap regression).
            } else if partial_slides.contains_key(&v) {
                // §9.5: the rim visits this un-beveled edge's SLIDE point (Blender's cap n-gon is
                // [arc…, one slide per remaining edge] in fan order). A flanking edge's slide point
                // equals the arc endpoint already appended → `dedup_ring` drops the repeat.
                if let Some(&si) = slide_idx.get(&(v, ekey_of(ce, mesh))) {
                    if loop_pts.last() != Some(&si) { loop_pts.push(si); }
                }
            } else if !center_idx.contains_key(&v) && !span_round_verts.contains(&v) {
                // (false,false) at a NON-partial (k≥2) corner: the persisting original vertex is part of the
                // rim (matches STEP-3's re-added V). At a PARTIAL corner V is DELETED — its rim is just the
                // arc, fanned to the recessed center C below (the center is supplied by the re-traced faces).
                // At a SPAN corner the bracketing beveled arcs already SHARE the across-span BoundVert (their
                // welded endpoints coincide), so the chain is clean WITHOUT inserting V — skip it.
                let vidx = weld_orig(v, &mut weld, &mut orig_vert_idx);
                if loop_pts.last() != Some(&vidx) { loop_pts.push(vidx); }
            }
        }
        dedup_ring(&mut loop_pts);
        // A PARTIAL corner fans its OPEN rim to the recessed center C, so a 2-point rim (the FLAT seg=1
        // chamfer — the octahedron-base case) is valid: [rim0, rim1, C] is one triangle. A NON-partial
        // (k≥2 n-gon / centroid-fan) rim genuinely needs ≥3. Gate accordingly.
        let min_rim = if center_idx.contains_key(&v) { 2 } else { 3 };
        let _ = &slide_idx;
        if loop_pts.len() < min_rim { continue; }

        // Orient the cap to TWIN the strips. CONV-B strip `q_i` traverses its arc FORWARD at the edge's
        // SRC endpoint but BACKWARD at its DST endpoint, so the cap must run that arc segment the opposite
        // way. Find one rim segment that is a beveled-edge arc segment, read whether THIS vertex is that
        // edge's src or dst, and flip the whole rim to the twin-correct sense (a consistent rim cycle has
        // ONE correct global orientation → fixing one segment fixes all). vn is the fallback for a rim
        // with no arc segment.
        let cn = loop_pts.len();
        let mut oriented = false;
        'orient: for i in 0..cn {
            let (a, b) = (loop_pts[i], loop_pts[(i + 1) % cn]);
            for &ce in &fan {
                if !beveled_he.contains(&ce.0) { continue; }
                let prim = if _primary_set.contains(&ce.0) { ce.0 } else { mesh.he(ce).twin.0 };
                let v_is_src = mesh.he_source(HalfEdgeId(prim)).0 == v;
                if let Some(arc) = arc_at.get(&(prim, v)) {
                    for w in 0..arc.len().saturating_sub(1) {
                        let fwd = arc[w] == a && arc[w + 1] == b;
                        let bwd = arc[w] == b && arc[w + 1] == a;
                        if fwd || bwd {
                            let cap_forward = !v_is_src; // FORWARD at a dst-corner, BACKWARD at a src-corner
                            if fwd != cap_forward { loop_pts.reverse(); }
                            oriented = true;
                            break 'orient;
                        }
                    }
                }
            }
        }
        if !oriented {
            let nl = newell_idx(&loop_pts, &weld.positions);
            let sign_ref = if vn0.length_squared() > 1e-8 { vn0 } else { nl };
            if nl.dot(sign_ref) < 0.0 { loop_pts.reverse(); }
        }

        // A CLEAN apex rounds through the M_ADJ grid: fully-beveled (k==degree — every rim segment is a
        // profile arc), or MIXED with k≥3 and every unbeveled edge ISOLATED in the fan (its flanking rails
        // terminal-slide onto it → an on-edge BoundVert; no (false,false) sector inserts V into the rim —
        // the oracle-verified k=3-of-4 case, `mixed_k3d4_ref.json`). The rim is then the clean chained
        // profile arcs. (A 2D-convexity test wrongly rejects these STAR-projected rims — the topological
        // test is the right gate.) Everything else (k<2, k=2 non-weld, (false,false) sectors) stays flat.
        let clean_apex = {
            let d = fan.len();
            let bev: Vec<bool> = fan.iter().map(|h| {
                let tw = mesh.he(*h).twin;
                beveled_he.contains(&h.0) || (tw != NO_TWIN && beveled_he.contains(&tw.0))
            }).collect();
            let k = bev.iter().filter(|b| **b).count();
            let two_consec_unbev = (0..d).any(|i| !bev[i] && !bev[(i + 1) % d]);
            // SPAN corners round via the grid too: their across-span BoundVert (STEP 2b) made the rim a
            // clean count-k chain despite the (false,false) sector.
            k == d || (k >= 3 && !two_consec_unbev) || span_round_verts.contains(&v)
        };
        if partial_slides.contains_key(&v) {
            // §9.5 PARTIAL CORNER, Blender-faithful: ONE closed n-gon over [arc…, slide points].
            // (Oracle: the owner's degree-5 endpoint gives a hexagon = 4 profile points + 2 slides.)
            faces_out.push(loop_pts.clone());
            faces_mats.push(cap_mat);
        } else if let Some(&ci) = center_idx.get(&v) {
            // PARTIAL corner (Blender-matched): the rim is the OPEN arc; fan it to the shared recessed
            // center C. NO wrap-around edge — the arc's two endpoints connect to C through the re-traced
            // neighbour faces (which also reference C), so those C↔endpoint edges twin the fan's first/last
            // radial. This deletes V (no fold) and recesses C inward (no spike).
            for i in 0..loop_pts.len().saturating_sub(1) {
                faces_out.push(vec![loop_pts[i], loop_pts[i + 1], ci]);
                faces_mats.push(cap_mat);
            }
        } else if n_seg > 1 && clean_apex {
            // ROUNDED corner — Blender's M_ADJ converging QUAD GRID (§3.7, `fill_corner_grid_madj`,
            // oracle-verified vertex-exact on the cube at seg 2–5). A CLEAN fully-beveled apex
            // (`clean_apex`, k==degree≤4 — cube corner, pyramid / cylinder-cap apex, incl. degree>3
            // re-routed off STEP 2b). The rim (chained profile ARCS — SHARED with the strips) is pinned,
            // so each edge fillet's cross-section is preserved → edges stay PARALLEL. Non-clean corners
            // (k<degree, bevel-on-beveled) fall through to the FLAT centroid fan (a domed fan
            // self-intersects there, 8/36 verified).
            let nb = loop_pts.len();
            let pts: Vec<Vec3> = loop_pts.iter().map(|&i| weld.positions[i]).collect();
            let rc: Vec3 = pts.iter().copied().sum::<Vec3>() / nb as f32;
            let up_dir = (orig_pos[v as usize] - rc).normalize_or_zero();
            // The rim lies on the corner's fillet sphere; by symmetry that sphere's centre is ON the axis
            // (rc, up_dir), so fit its CIRCLE in the (perp-radius, axial-height) plane — a robust 2×2
            // least-squares. (The full 3D sphere fit is ill-conditioned for a rim that's a shallow
            // spherical cap and overshoots the pole.) Model: u = perp²+h² = 2·h·c_h + K, K = r²−c_h².
            // Pole = centre + r·up → Blender's EXACT centre (0.873³ on the ±1 cube). Falls back to the
            // rho estimate if the rim is a single ring (all h equal → singular).
            let fit: Option<(Vec3, f32)> = {
                let (mut s_hh, mut s_h, mut s_hu, mut s_u) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
                for p in &pts {
                    let d = *p - rc;
                    let h = d.dot(up_dir);
                    let u = (d - up_dir * h).length_squared() + h * h;
                    s_hh += h * h; s_h += h; s_hu += h * u; s_u += u;
                }
                let nf = nb as f32;
                let det = 4.0 * s_hh * nf - 4.0 * s_h * s_h; // 2×2 normal-eq determinant
                if det.abs() > 1e-9 {
                    let c_h = (nf * 2.0 * s_hu - 2.0 * s_h * s_u) / det;
                    let kk = (4.0 * s_hh * s_u - 2.0 * s_h * 2.0 * s_hu) / det;
                    let r2 = kk + c_h * c_h;
                    if r2 > 1e-9 { Some((rc + up_dir * c_h, r2.sqrt())) } else { None }
                } else { None }
            };
            let apex = fit.map(|(c, r)| c + up_dir * r).unwrap_or_else(|| {
                let rho = { let mut s = 0.0f32;
                    for p in &pts { let d = *p - rc; s += (d - up_dir * d.dot(up_dir)).length(); }
                    s / nb as f32 };
                let r_sphere = w.max(rho * 1.02);
                let depth = (r_sphere * r_sphere - rho * rho).max(0.0).sqrt();
                rc + up_dir * (r_sphere - depth)
            });
            if !fill_corner_grid_madj(&loop_pts, n_seg, orig_pos[v as usize], apex, fit, shape,
                &bv_set, &mut weld, &mut faces_out)
            {
                // Rim is not the clean n×n_seg arc chain — fillet-sphere FAN fallback (parallel rim,
                // no spike, watertight; just fan topology instead of the grid).
                let ci = weld.push(apex);
                for i in 0..nb {
                    faces_out.push(vec![loop_pts[i], loop_pts[(i + 1) % nb], ci]);
                    faces_mats.push(cap_mat);
                }
            } else {
                pad_face_mats(&faces_out, &mut faces_mats, cap_mat);
            }
        } else if is_simple_loop(&loop_pts, &weld.positions) {
            // FLAT k≥2 corner (seg=1), simple rim → one n-gon, oriented by strip-match (Newell outward).
            faces_out.push(loop_pts);
            faces_mats.push(cap_mat);
        } else {
            // FLAT k≥2 non-planar rim → centroid fan (flush, no spike; every triangle simple).
            let rc: Vec3 = loop_pts.iter().map(|&i| weld.positions[i]).sum::<Vec3>() / loop_pts.len() as f32;
            let ci = weld.push(rc);
            let c = loop_pts.len();
            for i in 0..c {
                faces_out.push(vec![loop_pts[i], loop_pts[(i + 1) % c], ci]);
                faces_mats.push(cap_mat);
            }
        }
    }

    // ARC INNER MITER — the corner patches built alongside STEP 2.5, emitted here with the other
    // corner fills. Their rims are already welded to the strips and the wall runs, so they need no
    // rim walk of their own.
    if !patch_faces.is_empty() {
        let mat = miter_edges
            .keys()
            .copied()
            .min()
            .map(|v| vertex_bevel_material(mesh, v))
            .unwrap_or(0);
        for f in std::mem::take(&mut patch_faces) {
            faces_out.push(f);
        }
        pad_face_mats(&faces_out, &mut faces_mats, mat);
    }

    // ---------------------------------------------------------------------------------------------
    // STEP 5b — N-WAY MITER CAP (Blender `build_vmesh` M_POLY, seg=1). A degree>3 vertex with 3+ beveled
    // edges is a miter vertex (STEP 2b): V is deleted, replaced by its fan-ordered BoundVert ring. Unlike
    // the 2-edge miter (a bare edge, no cap), the ring must be FILLED — Blender emits one n-gon over it.
    // Each ring position welds to the SAME index the strips already reference (WeldMap dedups by position),
    // so the cap twins the strips → watertight. Orient the n-gon outward (Newell vs the vertex normal, the
    // same sense the deleted apex faces had). seg>1 rounded fill (M_ADJ grid) is a later extension (§3.7/§9).
    // ---------------------------------------------------------------------------------------------
    {
        let mut mv: Vec<u32> = miter_cap_rings.keys().copied().collect();
        mv.sort_unstable(); // deterministic face IDs
        for v in mv {
            let mut ring: Vec<usize> = miter_cap_rings[&v].iter().map(|&p| weld.push(p)).collect();
            dedup_ring(&mut ring);
            if ring.len() < 3 { continue; }
            let vn = mesh.compute_vertex_normal(crate::half_edge::VertexId(v));
            let nl = newell_idx(&ring, &weld.positions);
            let sign_ref = if vn.length_squared() > 1e-8 { vn } else { nl };
            if nl.dot(sign_ref) < 0.0 { ring.reverse(); }
            let cap_mat = vertex_bevel_material(mesh, v);
            faces_out.push(ring);
            faces_mats.push(cap_mat);
        }
    }

    // ---------------------------------------------------------------------------------------------
    // INSTALL
    // ---------------------------------------------------------------------------------------------
    LAST_BEVEL_NEW_FACE_START.with(|c| c.set(new_face_start));
    if use_local {
        // O(dirty) — measurement C path (large meshes).
        LAST_BEVEL_NEW_FACE_END.with(|c| c.set(faces_out.len() as u32));
        install_bevel_region(
            mesh,
            &affected_faces,
            &beveled_vertices,
            &weld,
            &orig_vert_idx,
            &faces_out,
            &faces_mats,
        );
        return;
    }

    // Full rebuild — oracle / multi-stage HE-id path (BEVEL.md: preserve soup renumbering).
    let input_closed = !(0..mesh.half_edges.len() as u32).any(|i| {
        let he = mesh.he(HalfEdgeId(i));
        he.face != NO_FACE && he.twin == NO_TWIN
    });
    if input_closed {
        let mut edge_seen: HashSet<(usize, usize)> = HashSet::new();
        for face in &faces_out {
            let k = face.len();
            for i in 0..k {
                edge_seen.insert((face[i], face[(i + 1) % k]));
            }
        }
        let mut boundary_edges: Vec<(usize, usize)> = edge_seen
            .iter()
            .copied()
            .filter(|&(a, b)| !edge_seen.contains(&(b, a)))
            .collect();
        boundary_edges.sort_unstable();
        let mut nxt: HashMap<usize, usize> = HashMap::new();
        for &(a, b) in &boundary_edges {
            nxt.insert(a, b);
        }
        let mut visited: HashSet<usize> = HashSet::new();
        let mut starts: Vec<usize> = nxt.keys().copied().collect();
        starts.sort_unstable();
        for s in starts {
            if visited.contains(&s) {
                continue;
            }
            let mut loop_v: Vec<usize> = Vec::new();
            let mut cur = s;
            loop {
                if visited.contains(&cur) {
                    break;
                }
                visited.insert(cur);
                loop_v.push(cur);
                match nxt.get(&cur) {
                    Some(&n) => cur = n,
                    None => break,
                }
                if cur == s {
                    break;
                }
            }
            if loop_v.len() >= 3 {
                // ANNOUNCE IT (§11 2026-08-15). This seal is a LAST RESORT: it closes whatever
                // boundary the builders left behind with ONE FLAT n-gon, which keeps the result
                // watertight but is only the right SHAPE where the corner really is flat. It
                // silently stood in for a missing rounded cap for months — the result passed
                // `bevel_result_installable` and the owner saw a broken round with no error
                // anywhere. A seal firing at seg>1 means some corner builder declined its rim;
                // log it so the next instance of that class is visible instead of invisible.
                if n_seg > 1 {
                    let ctr: Vec3 = loop_v.iter().map(|&i| weld.positions[i]).sum::<Vec3>()
                        / loop_v.len() as f32;
                    eprintln!(
                        "[BEVEL] SEAL: closed a {}-gon residual boundary at ({:.3},{:.3},{:.3}) \
                         with a FLAT n-gon at seg={n_seg} — a corner fill declined its rim \
                         (BEVEL.md §11 2026-08-15)",
                        loop_v.len(), ctr.x, ctr.y, ctr.z
                    );
                }
                loop_v.reverse();
                faces_out.push(loop_v);
                let seal_mat = mesh
                    .faces
                    .iter()
                    .find(|f| f.material_id > 0)
                    .map(|f| f.material_id)
                    .unwrap_or(0);
                faces_mats.push(seal_mat);
            }
        }
    }
    LAST_BEVEL_NEW_FACE_END.with(|c| c.set(faces_out.len() as u32));
    let mut out = HalfEdgeMesh::from_polygons(&weld.positions, &faces_out);
    apply_face_materials(&mut out, &faces_mats);
    *mesh = out;
}

/// Apply a dirty-region bevel soup to a live mesh: delete affected faces, allocate new rail verts,
/// install rings with boundary twin re-pairing, free orphaned beveled corners, region normals.
fn install_bevel_region(
    mesh: &mut HalfEdgeMesh,
    affected_faces: &HashSet<u32>,
    beveled_vertices: &HashSet<u32>,
    weld: &WeldMap,
    orig_vert_idx: &HashMap<u32, usize>,
    faces_out: &[Vec<usize>],
    faces_mats: &[u32],
) {
    use glam::Vec3;
    // weld index → live VertexId (reuse original slots where STEP-3 called weld_orig).
    let mut id_of: Vec<Option<VertexId>> = vec![None; weld.positions.len()];
    for (&orig, &wi) in orig_vert_idx {
        if wi < id_of.len() {
            id_of[wi] = Some(VertexId(orig));
        }
    }
    let n_new = id_of.iter().filter(|s| s.is_none()).count();
    let n_he_est: usize = faces_out.iter().map(|r| r.len()).sum();
    let first_new_vid = mesh.vertices.len() as u32;
    mesh.reserve_elements(n_new, n_he_est.saturating_mul(2), faces_out.len());
    for (i, slot) in id_of.iter_mut().enumerate() {
        if slot.is_none() {
            let p = weld.positions[i];
            *slot = Some(mesh.alloc_vertex(HVertex {
                position: p,
                normal: Vec3::Y,
                half_edge: NO_EDGE,
            }));
        }
    }

    let mut boundary: HashMap<(u32, u32), HalfEdgeId> = HashMap::new();
    let del: Vec<FaceId> = affected_faces.iter().map(|&i| FaceId(i)).collect();
    mesh.delete_faces_region(del, affected_faces, &mut boundary);

    let rings: Vec<(Vec<VertexId>, u32)> = faces_out
        .iter()
        .zip(faces_mats.iter())
        .filter_map(|(ring, &mat)| {
            if ring.len() < 3 {
                return None;
            }
            let vids: Vec<VertexId> = ring
                .iter()
                .map(|&i| id_of[i].expect("weld index must be mapped"))
                .collect();
            Some((vids, mat))
        })
        .collect();

    mesh.install_face_rings_local(&rings, boundary);

    // Drop unused candidates only (O(touched)) — full face scan would re-introduce O(mesh).
    let mut used: HashSet<u32> = rings
        .iter()
        .flat_map(|(r, _)| r.iter().map(|v| v.0))
        .collect();
    let mut candidates: HashSet<u32> = beveled_vertices.clone();
    for (&o, _) in orig_vert_idx {
        candidates.insert(o);
    }
    for i in first_new_vid..mesh.vertices.len() as u32 {
        candidates.insert(i);
    }
    for &v in &candidates {
        let vid = VertexId(v);
        if (v as usize) >= mesh.vertices.len() {
            continue;
        }
        let h = mesh.vert(vid).half_edge;
        if mesh.is_edge_live(h) && mesh.he_source(h) == vid && mesh.he(h).face != NO_FACE {
            used.insert(v);
        }
    }
    for &v in &candidates {
        if !used.contains(&v) {
            let vid = VertexId(v);
            if (v as usize) < mesh.vertices.len() {
                mesh.vert_mut(vid).half_edge = NO_EDGE;
                mesh.free_vert(vid);
            }
        }
    }

    let mut seed: Vec<VertexId> = Vec::new();
    for (r, _) in &rings {
        seed.extend(r.iter().copied());
    }
    mesh.recompute_normals_region(seed.iter().copied());

    debug_assert!(
        mesh.validate().is_ok(),
        "bevel local install invalid: {:?}",
        mesh.validate()
    );
}



// offset primitives → offsets.rs (order_arc..isect_line_plane)

// corner miter arc → corners.rs (miter_weld_arc)

// offset primitives → offsets.rs (inward_offset_line..drop_onto_faces)

// corner boundary → corners.rs (build_boundary_flat, partial_back_edge_slide)

// profile primitives → offsets.rs (quad_bezier, circular_arc)

// caps / M_ADJ grid → caps.rs

/// Copy per-output-face materials onto a mesh rebuilt by [`HalfEdgeMesh::from_polygons`]
/// (output face order matches the polygon soup order). Same pattern as `loop_cut_multi` /
/// `triangulate_mesh` — without this, every beveled face drops to `material_id = 0` and the
/// assigned graph material disappears in the viewport.
fn apply_face_materials(mesh: &mut HalfEdgeMesh, mats: &[u32]) {
    for (i, &m) in mats.iter().enumerate() {
        if i < mesh.faces.len() {
            mesh.faces[i].material_id = m;
        }
    }
}

/// Material of a face slot, or 0 for free / invalid faces.
fn face_material_id(mesh: &HalfEdgeMesh, fid: FaceId) -> u32 {
    if fid == NO_FACE || fid.0 as usize >= mesh.faces.len() {
        0
    } else {
        mesh.face(fid).material_id
    }
}

/// Prefer a non-zero material among faces incident to `v` (bevel strip/cap inheritance).
fn vertex_bevel_material(mesh: &HalfEdgeMesh, v: u32) -> u32 {
    // Face-outgoings (not incomplete boundary fan) so plane corners still inherit a material.
    let mut first = 0u32;
    for &h in &vertex_face_outgoings(mesh, VertexId(v)) {
        let f = mesh.he(h).face;
        if f == NO_FACE {
            continue;
        }
        let m = mesh.face(f).material_id;
        if first == 0 {
            first = m;
        }
        if m > 0 {
            return m;
        }
    }
    first
}

/// Pad `faces_mats` so it matches `faces_out.len()` (for batch emitters like the M_ADJ grid).
fn pad_face_mats(faces_out: &[Vec<usize>], faces_mats: &mut Vec<u32>, mat: u32) {
    while faces_mats.len() < faces_out.len() {
        faces_mats.push(mat);
    }
}

// cap rim helpers → caps.rs (dedup_ring..is_simple_loop)

/// Number of boundary LOOPS (connected components of the boundary-edge graph) in `mesh`.
///
/// A boundary edge here is a FACE-bearing half-edge with no twin (the kernel convention the app's
/// bevel log already uses). A closed mesh has 0 loops; a mesh with one open rim / hole has 1; N
/// disjoint holes → N. This is the TOPOLOGICALLY MEANINGFUL boundary metric for the bevel install
/// guard (`bevel_result_installable`).
///
/// Why loops, not the boundary EDGE count the old guard used: a LEGITIMATE bevel of an interior edge
/// whose endpoint lies on an open rim RESHAPES the existing rim — it adds a chamfer strip along the
/// open side, so the boundary EDGE count legitimately GROWS (Blender's own output does the same:
/// openbox interior edge, boundary 4→5 flat / 4→7 seg3). But it never SPAWNS a new hole, so the loop
/// count is stable. A TEAR (the closed-mesh corner/cap machinery cracking a mesh that should stay
/// watertight) opens a spurious hole → the loop count GROWS. So `loops(new) <= loops(orig)` accepts
/// every faithful open-mesh bevel and still rejects tears, whereas `boundary_edges(new) <= orig`
/// wrongly rejected EVERY open-mesh bevel that touched the rim (the "bevel doesn't work on open
/// faces" bug). For a closed mesh loops==0 iff boundary_edges==0, so the closed-mesh behaviour is
/// unchanged.
pub fn boundary_loop_count(mesh: &HalfEdgeMesh) -> usize {
    // Union-find over the vertices joined by boundary edges; component count = loop count (a manifold
    // boundary vertex has exactly 2 boundary edges, so each component is a simple closed loop).
    fn find(parent: &mut HashMap<u32, u32>, x: u32) -> u32 {
        let mut r = x;
        while let Some(&p) = parent.get(&r) { if p == r { break; } r = p; }
        // path-halving for stability on large rims
        let mut cur = x;
        while let Some(&p) = parent.get(&cur) { if p == r { break; } parent.insert(cur, r); cur = p; }
        r
    }
    let mut parent: HashMap<u32, u32> = HashMap::new();
    let mut any = false;
    for i in 0..mesh.half_edges.len() as u32 {
        let he = mesh.he(HalfEdgeId(i));
        if he.twin == NO_TWIN && he.face != NO_FACE {
            any = true;
            let s = mesh.he_source(HalfEdgeId(i)).0;
            let d = he.vertex.0;
            parent.entry(s).or_insert(s);
            parent.entry(d).or_insert(d);
            let (ra, rb) = (find(&mut parent, s), find(&mut parent, d));
            if ra != rb { parent.insert(ra, rb); }
        }
    }
    if !any { return 0; }
    let keys: Vec<u32> = parent.keys().copied().collect();
    let mut roots: HashSet<u32> = HashSet::new();
    for k in keys { let r = find(&mut parent, k); roots.insert(r); }
    roots.len()
}

/// Decide whether a beveled result `new` (built from `orig`) is safe to INSTALL, for BOTH closed and
/// open input meshes. THE canonical bevel install guard — call this from the app instead of an
/// ad-hoc `validate() && boundary_edges <= orig` check.
///
/// Rejects a result that TORE:
///   • non-manifold output — `is_manifold()` (stricter than the old `validate()`, and it ACCEPTS a
///     clean OPEN manifold mesh, so open inputs aren't rejected for being open); catches the
///     non-manifold-edge / bowtie / degenerate-face cracks the closed-mesh machinery can produce on
///     an unsupported corner (e.g. the N-way miter the arm-time toast warns about);
///   • a NEW boundary loop — `boundary_loop_count(new) > boundary_loop_count(orig)` = a spurious hole.
///
/// Accepts a faithful bevel:
///   • closed input: loops stay 0 (== the old `boundary_edges == 0` behaviour — no regression);
///   • OPEN input: the rim is reshaped (boundary edge count grows) but the loop count is preserved —
///     the case the old edge-count guard wrongly rejected.
///
/// RELATIVE, never absolute (owner ruling 2026-07-18: "bevel need to work even if the mesh is open
/// or broken"). A mesh that is ALREADY non-manifold must stay editable — requiring `new` to be
/// manifold outright meant one defect anywhere (e.g. an imported or historically-corrupted mesh)
/// silently disabled bevel EVERYWHERE. So the manifold check applies only when the INPUT was
/// manifold: clean meshes keep full tear protection; broken meshes are held to "no new holes" only.
pub fn bevel_result_installable(orig: &HalfEdgeMesh, new: &HalfEdgeMesh) -> bool {
    let manifold_ok = if orig.is_manifold().is_ok() { new.is_manifold().is_ok() } else { true };
    manifold_ok && boundary_loop_count(new) <= boundary_loop_count(orig)
}

#[cfg(test)]
mod tests;
