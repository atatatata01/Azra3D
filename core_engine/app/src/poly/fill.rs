use std::sync::Arc;
use glam::Vec3;
use core_math::half_edge::{
    FaceId, HVertex, HalfEdgeId, HalfEdgeMesh, VertexId, NO_EDGE, NO_FACE, NO_TWIN,
};
use core_math::scene_graph::{NodeId, NodePayload};
use crate::{AzraApp, DirtyFlags};

impl AzraApp {
    /// F key — Fill: create a face from selected vertices (3+ required).
    ///
    /// FILL1: the raw `HashSet<u32>` iteration order is hash-seed-dependent, so feeding
    /// it straight to `create_face` produced bowtie (self-intersecting) faces and a
    /// run-to-run-varying normal. We instead derive a deterministic cyclic order by
    /// angular sort in the best-fit plane, then orient the winding toward the camera
    /// (same convention as the Bridge tool). create_face stays a faithful primitive.
    pub(crate) fn poly_fill_face(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        // Accept an EDGE selection too, not just vertices.
        //
        // Owner 2026-07-30: "I did not see the command to create a connected poly from selection."
        // Fill existed but read ONLY `selected_vertices` and was gated to Vertex mode
        // (`key_dispatch.rs` + `WHEN_POLY_VERT`), so in Edge mode — where the user had just selected
        // a boundary loop — there was no way to reach it and it never appeared in the Edge menu.
        // Blender's F fills from selected edges as well; deriving the verts from the edge selection
        // is all that was missing.
        let mut verts: Vec<u32> = self.poly_sel.selected_vertices.iter().copied().collect();
        if verts.len() < 3 && !self.poly_sel.selected_edges.is_empty() {
            let mut from_edges: std::collections::HashSet<u32> = std::collections::HashSet::new();
            if let Some(node) = self.scene.scene_graph.get(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                    for &e in &self.poly_sel.selected_edges {
                        let he = core_math::half_edge::HalfEdgeId(e);
                        if !mesh.is_edge_live(he) { continue; }
                        from_edges.insert(mesh.he_source(he).0);
                        from_edges.insert(mesh.he(he).vertex.0);
                    }
                }
            }
            if from_edges.len() >= 3 {
                verts = from_edges.into_iter().collect();
            }
        }
        // QUAD FILL (F2-style): exactly ONE vertex selected and no usable edge selection. Either
        // the corners already exist on the open border (closing) or a 4th is invented and a quad
        // grows off the border (extend) — refusing is the last resort, not the first answer.
        // Everything below this branch is the pre-existing 3+ vertex path, untouched.
        if verts.len() == 1 {
            self.poly_quad_fill_at_vertex(nid, VertexId(verts[0]));
            return;
        }
        if verts.len() < 3 {
            self.toast("Fill: select 3+ vertices, or an edge loop");
            return;
        }
        let cam_forward = self.viewport.camera.orbit * Vec3::new(0.0, 0.0, -1.0);

        // Dry-run: refuse without undo if collinear / create_face fails (circle/inset pattern).
        // `transaction()` always pushes — a failed fill must not eat Ctrl+Z.
        let ring = {
            let Some(node) = self.scene.scene_graph.get(nid) else { return };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else { return };
            match try_fill_ring(mesh, &verts, cam_forward) {
                FillTry::Ok(r) => r,
                FillTry::Degenerate => {
                    self.toast("Could not create face (vertices are collinear)");
                    return;
                }
                FillTry::Unsafe => {
                    self.toast("Fill: that face would break the mesh (it welds to nothing)");
                    return;
                }
                FillTry::Failed => {
                    self.toast("Could not create face");
                    return;
                }
            }
        };

        self.transaction("Fill Face", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let mesh = Arc::make_mut(mesh);
                    if let Some(fid) = mesh.create_face(&ring) {
                        s.poly_sel.selected_faces.insert(fid.0);
                        s.poly_request_display_sync(nid);
                        s.mark_dirty(DirtyFlags::RENDER);
                        s.toast("Face created");
                    } else {
                        // Race: mesh changed between dry-run and apply (should be rare).
                        s.toast("Could not create face");
                    }
                }
            }
        });
    }

    /// `F` with exactly ONE vertex selected — Quad Fill
    /// (`docs/planning/poly/QUAD_FILL_SINGLE_VERTEX_PLAN.md` §3, phases P2 and P4).
    ///
    /// Two modes, both emitting at most ONE face, DECIDED rather than guessed (plan §2.2):
    ///
    /// * **Closing (P2)** — the corners of the gap already exist on the open border, so the op
    ///   walks to them and welds one face. Capped at a QUAD: a longer hole fills the quad nearest
    ///   the selected vertex and stays open, so repeated presses zip it shut (owner decision §0).
    /// * **Extend (P4)** — the border cannot supply a 4th corner, so one is INVENTED and a quad
    ///   grows off the border into the empty space. Placement rule: [`try_extend_quad`].
    ///
    /// Whole-loop filling belongs to `Ctrl+Alt+F` Grid Fill and must not be duplicated here; an
    /// n-gon is never emitted.
    ///
    /// **The mouse does less work in extend mode** (plan §2.3), and this method is where the
    /// asymmetry lives. Closing walks ONE way round the rim, so P3's cursor test is what will
    /// choose the side. Extend consumes BOTH of the vertex's boundary neighbours, so there is no
    /// side left to choose — the cursor will only ever disambiguate an extend when the vertex
    /// carries more than two boundary edges (a pinch), which [`try_extend_quad`] declines today
    /// rather than picking a sector at random.
    ///
    /// Every decline returns BEFORE `transaction()` opens, because `transaction()` always pushes
    /// an undo entry — a refused fill must not eat the user's Ctrl+Z.
    fn poly_quad_fill_at_vertex(&mut self, nid: NodeId, v: VertexId) {
        let planned = {
            let Some(node) = self.scene.scene_graph.get(nid) else { return };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else { return };
            match plan_quad_fill(mesh, v) {
                Ok(p) => p,
                Err(why) => {
                    self.toast(why.toast());
                    return;
                }
            }
        };
        let msg = planned.toast();

        self.transaction("Quad Fill", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let mesh = Arc::make_mut(mesh);
                    // Extend stages its invented corner HERE, through the very function the dry
                    // run proved on the clone — the proof and the apply are one construction, so
                    // there is no second place for the ring to be built differently.
                    let (ring, invented) = match &planned {
                        QuadFillPlan::Closing { ring, .. } => (ring.clone(), None),
                        QuadFillPlan::Extend { plan, .. } => {
                            let ring = stage_extend_quad(mesh, plan);
                            let x = ring.last().copied();
                            (ring, x)
                        }
                    };
                    if let Some(fid) = mesh.create_face(&ring) {
                        s.poly_sel.selected_faces.insert(fid.0);
                        // P3: move the single-vertex selection to the next border vertex in the
                        // walked direction, so a second `F` zips the next quad instead of
                        // re-testing the same corner.
                        s.poly_request_display_sync(nid);
                        s.mark_dirty(DirtyFlags::RENDER);
                        s.toast(msg);
                    } else {
                        // Race: mesh changed between dry-run and apply (should be rare). Retire
                        // the invented corner instead of leaving a loose vertex behind — a fill
                        // that produced no face must add no geometry either.
                        if let Some(x) = invented {
                            mesh.free_vert(x);
                        }
                        s.toast("Could not create face");
                    }
                }
            }
        });
    }
}

/// What a single-vertex `F` will do, already proven on a clone.
enum QuadFillPlan {
    /// Weld a face onto corners that ALREADY exist (closing mode, plan §3 P2).
    Closing {
        ring: Vec<VertexId>,
        /// The face welds soundly but lies OVER the surface rather than closing a gap in it —
        /// see [`face_folds_over_neighbour`]. A WARNING, never a refusal (owner 2026-08-09).
        over_surface: bool,
    },
    /// Grow a quad, inventing its far corner (extend mode, plan §3 P4).
    Extend { plan: ExtendPlan, over_surface: bool },
}

impl QuadFillPlan {
    /// The toast that reports what happened. Owner decision 2026-08-09: an over-surface fill is
    /// FILLED, not refused — the detector (`face_folds_over_neighbour`) false-negatives on a
    /// concave rim, so it warns instead of blocking. The toast has to NAME what happened, because
    /// the result is otherwise indistinguishable from a correct one until the user orbits.
    fn toast(&self) -> &'static str {
        match self {
            QuadFillPlan::Closing { ring, over_surface } => match (*over_surface, ring.len() == 4) {
                (true, true) => "Quad filled ON TOP of the surface — that border is an outer rim, not a hole. Ctrl+Z to undo.",
                (true, false) => "Triangle filled ON TOP of the surface — that border is an outer rim, not a hole. Ctrl+Z to undo.",
                (false, true) => "Quad filled",
                (false, false) => "Triangle filled",
            },
            QuadFillPlan::Extend { over_surface, .. } => {
                if *over_surface {
                    "Quad grown ON TOP of the surface — that border is an outer rim, so the new quad lies over it. Ctrl+Z to undo."
                } else {
                    "Quad grown"
                }
            }
        }
    }
}

/// Why a single-vertex `F` refused. Separate from its wording so the decision is testable
/// exactly and the phrasing lives in ONE place ([`Self::toast`]).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum QuadFillDecline {
    /// The vertex has no open border at all.
    NotOnBorder,
    ClosingFailed,
    /// Two or more borders meet at the vertex, so "the two boundary neighbours" is ambiguous.
    Pinch,
    /// Only one side of the border exists here — a parallelogram needs both neighbours.
    DeadEnd,
    /// The border runs straight through the vertex, so `A + E − V` lands back on `V`.
    Straight,
    /// The invented corner would land on a border corner that already exists.
    CornerExists,
    /// The grown quad would not twin with the border it was meant to grow from.
    WouldNotAttach,
    ExtendUnsafe,
    ExtendFailed,
}

impl QuadFillDecline {
    fn toast(self) -> &'static str {
        match self {
            QuadFillDecline::NotOnBorder => "Quad Fill: that vertex is not on an open border",
            QuadFillDecline::ClosingFailed => "Could not create face",
            QuadFillDecline::Pinch => "Quad Fill: two borders meet at that vertex — pick a vertex on just one of them",
            QuadFillDecline::DeadEnd => "Quad Fill: the border dead-ends at that vertex — a quad needs border on both sides of it",
            QuadFillDecline::Straight => "Quad Fill: the border runs straight through that vertex — a grown quad would have no width",
            QuadFillDecline::CornerExists => "Quad Fill: the fourth corner is already on the border — growing a second one there would crack the mesh",
            QuadFillDecline::WouldNotAttach => "Quad Fill: that quad would not attach to the border",
            QuadFillDecline::ExtendUnsafe => "Quad Fill: growing a quad there would break the mesh",
            QuadFillDecline::ExtendFailed => "Could not create face",
        }
    }
}

/// Decide what `F` does at `v` and PROVE it, or say why not. Read-only in `mesh`, so every
/// refusal leaves it byte-identical — which is what lets the caller return before `transaction()`
/// opens and therefore never eat a Ctrl+Z.
///
/// **Closing runs first and extend only picks up what it drops.** Extend is reached through the
/// four closing verdicts that all say the same thing — *the corners this border offers do not
/// make a fillable quad*:
///
/// * `ChainEnd` / `TooShort` / `Degenerate` — the plan names each of these as extend's case
///   (§2.2, §2.3, and `ClosingTry`'s own doc comments).
/// * `Unsafe` — **and this one is the owner's L-notch**, so it is not an afterthought. Where a
///   grid is missing its corner cell, the rim walk leaves the notch and comes back around the
///   OUTSIDE, and the ring it returns closes across an edge that is already interior. That welds
///   into a duplicated directed edge, which is exactly what `Unsafe` reports. Declining there
///   would leave the one shape extend exists for permanently unreachable. Falling through is not
///   "guessing at broken topology" either: extend proves its own quad from scratch
///   ([`try_extend_quad`] runs the same [`prove_weld_staged`] and additionally asserts the weld),
///   so the worst it can do is refuse in turn.
///
/// `Interior` does NOT fall through — a vertex with no border has nothing to grow from either —
/// and neither does `Failed`, which means `create_face` rejected the ring outright. A real
/// 4-corner hole is always filled with the vertices that already exist, never with a new one,
/// because closing is asked first and only its refusals reach here.
fn plan_quad_fill(mesh: &HalfEdgeMesh, v: VertexId) -> Result<QuadFillPlan, QuadFillDecline> {
    match try_closing_ring(mesh, v) {
        ClosingTry::Ok { ring, over_surface } => Ok(QuadFillPlan::Closing { ring, over_surface }),
        ClosingTry::Interior => Err(QuadFillDecline::NotOnBorder),
        ClosingTry::Failed => Err(QuadFillDecline::ClosingFailed),
        // The border has no 4th corner to offer — grow one (plan §3 P4).
        ClosingTry::ChainEnd
        | ClosingTry::TooShort
        | ClosingTry::Degenerate
        | ClosingTry::Unsafe => {
            match try_extend_quad(mesh, v) {
                ExtendTry::Ok { plan, over_surface } => {
                    Ok(QuadFillPlan::Extend { plan, over_surface })
                }
                ExtendTry::Interior => Err(QuadFillDecline::NotOnBorder),
                ExtendTry::Pinch => Err(QuadFillDecline::Pinch),
                ExtendTry::OneSided => Err(QuadFillDecline::DeadEnd),
                ExtendTry::Straight => Err(QuadFillDecline::Straight),
                ExtendTry::CornerExists => Err(QuadFillDecline::CornerExists),
                ExtendTry::NoWeld => Err(QuadFillDecline::WouldNotAttach),
                ExtendTry::Unsafe => Err(QuadFillDecline::ExtendUnsafe),
                ExtendTry::Failed => Err(QuadFillDecline::ExtendFailed),
            }
        }
    }
}

/// Why CLOSING was refused, or the ring that will close the gap. Four of these refusals are not
/// the end of the press — [`plan_quad_fill`] hands them on to extend mode; see its doc comment
/// for which and why.
enum ClosingTry {
    Ok {
        ring: Vec<VertexId>,
        /// The face welds soundly but lies OVER the surface rather than closing a gap in it
        /// — see [`face_folds_over_neighbour`]. A WARNING, never a refusal (owner 2026-08-09).
        over_surface: bool,
    },
    /// Not on an open border at all. The ONE closing refusal extend cannot help with.
    Interior,
    /// Only ONE border edge here — the border terminates, so there is no gap to close and the
    /// 4th corner would have to be invented. → extend ([`try_extend_quad`]).
    ChainEnd,
    /// The border closes in fewer than 3 corners — nothing that could be a face. → extend.
    TooShort,
    /// The corners are collinear (a straight open edge). Closing across them would emit a
    /// zero-area sliver; F2 grows a quad outward here instead. → extend.
    Degenerate,
    /// The dry run produced a mesh that is non-manifold where the original was not — which is
    /// what the owner's L-notch looks like from closing's side, because the rim walk leaves the
    /// notch and closes back across an already-interior edge. → extend.
    Unsafe,
    Failed,
}

/// Classify the border at `v` and build the ring that closes it — read-only, so every decline
/// leaves the mesh byte-identical.
fn try_closing_ring(mesh: &HalfEdgeMesh, v: VertexId) -> ClosingTry {
    let (out_he, in_he) = mesh.boundary_edges_at(v);
    let (out_he, in_he) = match (out_he, in_he) {
        (Some(o), Some(i)) => (o, i),
        (None, None) => return ClosingTry::Interior,
        _ => return ClosingTry::ChainEnd,
    };

    // P3: the mouse decides which side of `v` to close — project both candidate directions to
    // screen space and take the one nearer the cursor. Until then the choice is FIXED (never
    // arbitrary per-run): walk against the border's own direction, which yields the ring already
    // wound to weld. `out_he` is the other candidate and is deliberately unused for now.
    let _ = out_he;
    let Some(ring) = mesh.boundary_closing_ring(v, in_he, 4) else {
        return ClosingTry::TooShort;
    };

    // A straight border has no gap to close — refuse rather than weld a sliver. Same collinearity
    // test (and tolerance) the 3+ vertex path uses, so both agree on what "degenerate" means.
    let pts: Vec<Vec3> = ring.iter().map(|&x| mesh.vert(x).position).collect();
    if best_fit_plane_normal(&pts).length_squared() < 1e-10 {
        return ClosingTry::Degenerate;
    }

    // Prove the weld on a CLONE before opening a transaction — ONE implementation, shared with
    // the 3+ vertex path (see `prove_weld`).
    match prove_weld(mesh, &ring) {
        WeldProof::Failed => ClosingTry::Failed,
        WeldProof::Unsafe => ClosingTry::Unsafe,
        WeldProof::Ok { scratch, face } => ClosingTry::Ok {
            over_surface: face_folds_over_neighbour(&scratch, face),
            ring,
        },
    }
}

// ============================================================
// EXTEND MODE — plan §3 P4. Grow a quad off an open border, inventing its far corner.
// ============================================================

/// Extend mode's decision: the three EXISTING corners of the new quad plus the one it invents.
struct ExtendPlan {
    /// `v`'s boundary neighbour across the INCOMING rim half-edge (`A` in plan §2.3).
    a: VertexId,
    /// The selected vertex (`V`).
    v: VertexId,
    /// `v`'s boundary neighbour across the OUTGOING rim half-edge (`E` in plan §2.3).
    e: VertexId,
    /// The invented corner `X`, ALREADY projected onto the surface plane at `v`.
    x: Vec3,
    /// Surface normal at `v`. Seeds the new vertex so it shades with its neighbours instead of
    /// carrying the zero normal `alloc_vertex` would otherwise leave behind.
    normal: Vec3,
}

/// Why extend mode refused, or the quad it will grow.
enum ExtendTry {
    Ok {
        plan: ExtendPlan,
        /// Same meaning, same detector and same owner ruling as closing mode's flag — a WARNING,
        /// never a refusal. See [`face_folds_over_neighbour`].
        over_surface: bool,
    },
    /// No open border at the vertex at all.
    Interior,
    /// More than one border sector meets at the vertex — "the two boundary neighbours" is
    /// ambiguous and picking one silently would be a lie (plan §5b finding 3).
    Pinch,
    /// The border reaches the vertex from one side only, so there is no second neighbour to
    /// complete a parallelogram with.
    OneSided,
    /// `A`, `V` and `E` are collinear, so `A + E − V` collapses onto `V` — a zero-width quad.
    Straight,
    /// The invented corner lands on a border corner that already exists; growing a second vertex
    /// there would leave two coincident corners and a crack.
    CornerExists,
    /// The quad installed but did not twin with the border it grew from — a face welded to
    /// nothing (plan §5b findings 1 and 2).
    NoWeld,
    /// The quad installed onto a mesh it broke.
    Unsafe,
    Failed,
}

/// Grow a quad off the open border at `v`, inventing the corner the border cannot supply
/// (`QUAD_FILL_SINGLE_VERTEX_PLAN.md` §2.3 / §3 P4). Read-only in `mesh` — every refusal below
/// leaves it byte-identical, so a decline costs the user no undo entry.
///
/// **The placement rule.** With `A` and `E` the two boundary neighbours of `V`, the invented
/// corner is the parallelogram completion `X = A + E − V` — the Blender F2 addon's own rule —
/// and the new face is the ring `[E, V, A, X]`. That ring order is not cosmetic: it carries the
/// directed edges `E→V` and `V→A`, the exact reverses of the two rim half-edges at `V`, which is
/// what makes `create_face`'s twin lookup weld the quad onto the border instead of leaving it
/// floating (`euler.rs:510`; the same topological-winding rule `boundary_closing_ring` states).
/// `A→X` and `X→E` are new rim.
///
/// **`X` is then PROJECTED onto the surface plane at `V`**, because a bare 3D parallelogram
/// drifts off any curved border — the quad has to continue the surface, not fly off it. §2.3
/// says "the plane of the face adjacent to `V`"; at a corner the gap is flanked by TWO faces, and
/// picking one of them would make the result depend on which way the mesh happens to be wound, so
/// the plane bisects them ([`surface_normal_at_rim`]). Where both rim half-edges belong to the
/// same face — the common case, and the one §2.3 describes — that is identical to the spec.
///
/// **The mouse does no work here** (§2.3): both boundary neighbours are consumed, so unlike
/// closing mode there is no side to pick. Cursor direction (P3) would only ever disambiguate a
/// vertex carrying more than two boundary edges, and that case returns [`ExtendTry::Pinch`]
/// rather than guessing — `boundary_edges_at` answers for ONE sector only (plan §5b finding 3),
/// so silently taking its answer at a pinch would pick a border at random.
///
/// **Why the rim edges are collected by scan rather than by `boundary_edges_at`.** That helper
/// walks ONE fan sector seeded from the vertex's disk pointer, so it cannot see a second sector
/// (the pinch above) and it reports only one side whenever that walk is derailed — which is the
/// `ChainEnd` verdict that sent us here in the first place. [`rim_edges_at`] asks the question
/// the placement rule actually needs — *every* twin-less half-edge touching `v` — and can
/// therefore both rescue a derailed sector walk and detect the pinch it hides.
fn try_extend_quad(mesh: &HalfEdgeMesh, v: VertexId) -> ExtendTry {
    let (outs, ins) = rim_edges_at(mesh, v);
    if outs.is_empty() && ins.is_empty() {
        return ExtendTry::Interior;
    }
    if outs.len() > 1 || ins.len() > 1 {
        return ExtendTry::Pinch;
    }
    let (Some(&out_he), Some(&in_he)) = (outs.first(), ins.first()) else {
        return ExtendTry::OneSided;
    };

    let a = mesh.he_source(in_he);
    let e = mesh.he(out_he).vertex;
    if a == v || e == v || a == e {
        return ExtendTry::Straight; // a self-loop or a two-vertex rim: no corner to complete
    }
    let (pv, pa, pe) = (
        mesh.vert(v).position,
        mesh.vert(a).position,
        mesh.vert(e).position,
    );
    let (da, de) = (pa - pv, pe - pv);
    let (na, ne) = (da.normalize_or_zero(), de.normalize_or_zero());
    if na.length_squared() < 0.5 || ne.length_squared() < 0.5 {
        return ExtendTry::Straight; // a zero-length rim edge has no direction
    }
    // |na × ne| is the sine of the corner angle at V. Zero means A, V and E are collinear, and
    // then X = A + E − V lands back on V: the "quad" would be a zero-area sliver. Refuse rather
    // than weld one (same reason closing mode refuses a straight border).
    if na.cross(ne).length() < 1e-4 {
        return ExtendTry::Straight;
    }

    let x_raw = pa + pe - pv;
    let normal = surface_normal_at_rim(mesh, in_he, out_he);
    let x = if normal.length_squared() > 0.5 {
        x_raw - normal * (x_raw - pv).dot(normal)
    } else {
        x_raw // no usable adjacent face: the parallelogram plane is the only surface there is
    };

    // If the border ALREADY reaches that corner, the fill wanted was a closing one and inventing
    // a coincident vertex would leave a crack. Bounded: only the corner two rim steps out on each
    // side can be the missing one, so this is two short walks, not a mesh scan.
    let tol = 1e-3 * da.length().max(de.length()).max(1e-6);
    for start in [in_he, out_he] {
        for c in mesh.boundary_walk_from(v, start, 2) {
            if mesh.is_vert_live(c) && (mesh.vert(c).position - x).length() <= tol {
                return ExtendTry::CornerExists;
            }
        }
    }

    let plan = ExtendPlan { a, v, e, x, normal };
    match prove_weld_staged(mesh, |m| stage_extend_quad(m, &plan)) {
        WeldProof::Failed => ExtendTry::Failed,
        WeldProof::Unsafe => ExtendTry::Unsafe,
        WeldProof::Ok { scratch, face } => {
            // `prove_weld` catches a face that BREAKS the mesh, but a quad that merely fails to
            // twin breaks nothing — `create_face`'s twin pass is best-effort and silently leaves
            // `NO_TWIN` when `find_half_edge`'s fan misses a real edge (plan §5b findings 1+2),
            // which is precisely the boundary-vertex situation this op lives in. The two rim
            // half-edges losing their `NO_TWIN` IS the weld, so assert it directly.
            if scratch.he(in_he).twin == NO_TWIN || scratch.he(out_he).twin == NO_TWIN {
                return ExtendTry::NoWeld;
            }
            ExtendTry::Ok {
                over_surface: face_folds_over_neighbour(&scratch, face),
                plan,
            }
        }
    }
}

/// Install extend mode's quad: allocate the invented corner and return the ring
/// `[E, V, A, X]`. Called TWICE per press — once on [`prove_weld_staged`]'s clone and once on the
/// live mesh inside the transaction — so what was proven and what is applied are the same
/// construction, and the new vertex gets whichever id the mesh it is being built into hands out.
fn stage_extend_quad(mesh: &mut HalfEdgeMesh, plan: &ExtendPlan) -> Vec<VertexId> {
    let x = mesh.alloc_vertex(HVertex {
        position: plan.x,
        normal: plan.normal,
        half_edge: NO_EDGE,
    });
    vec![plan.e, plan.v, plan.a, x]
}

/// EVERY live boundary half-edge incident to `v`, split by direction: `(outgoing, incoming)` —
/// outgoing meaning sourced at `v`, incoming meaning ending at `v`.
///
/// The sector-walking [`HalfEdgeMesh::boundary_edges_at`] is the right tool for *following* a rim
/// (it stays inside the sector it was seeded in, which is what a walk needs). This is the right
/// tool for *counting* one: it answers for the whole vertex, so `(1, 1)` proves there is exactly
/// one border passing through and anything larger is a pinch. O(half_edges), on one key press —
/// the same shape of scan `vertex_is_boundary` already runs (`nav.rs:510`).
fn rim_edges_at(mesh: &HalfEdgeMesh, v: VertexId) -> (Vec<HalfEdgeId>, Vec<HalfEdgeId>) {
    let (mut outs, mut ins) = (Vec::new(), Vec::new());
    if !mesh.is_vert_live(v) {
        return (outs, ins);
    }
    for i in 0..mesh.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if !mesh.is_edge_live(h) || mesh.he(h).twin != NO_TWIN {
            continue;
        }
        if mesh.he_source(h) == v {
            outs.push(h);
        }
        if mesh.he(h).vertex == v {
            ins.push(h);
        }
    }
    (outs, ins)
}

/// The surface normal at the rim vertex the two half-edges meet at — the normal of the plane the
/// grown quad is projected onto (plan §2.3).
///
/// The two rim half-edges are the ends of the gap, so their faces are the surface on either side
/// of it. Averaging is what makes the answer independent of which one the mesh's winding calls
/// "incoming"; with both rim edges on one face (a lone quad, a strip end) it degenerates to that
/// face's own normal, which is exactly what §2.3 asks for. Opposed normals (a folded pair) sum to
/// zero — fall back to the first real one rather than returning a direction of zero length.
fn surface_normal_at_rim(mesh: &HalfEdgeMesh, in_he: HalfEdgeId, out_he: HalfEdgeId) -> Vec3 {
    let mut sum = Vec3::ZERO;
    let mut first = Vec3::ZERO;
    for h in [in_he, out_he] {
        let f = mesh.he(h).face;
        if f == NO_FACE || !mesh.is_face_live(f) {
            continue;
        }
        let n = mesh.compute_face_normal(f);
        if n.length_squared() < 0.5 {
            continue; // a degenerate face has no plane to continue
        }
        if first == Vec3::ZERO {
            first = n;
        }
        sum += n;
    }
    let n = sum.normalize_or_zero();
    if n.length_squared() < 0.5 {
        first.normalize_or_zero()
    } else {
        n
    }
}

/// A candidate ring proven on a CLONE, or the reason it was refused.
enum WeldProof {
    /// The ring welds soundly. Carries the proven clone and the face id it created, so a caller
    /// that has to inspect the RESULT (the outer-rim fold test) does not clone the mesh twice.
    Ok { scratch: HalfEdgeMesh, face: FaceId },
    /// `create_face` refused the ring outright.
    Failed,
    /// The face installed, but onto a mesh it broke.
    Unsafe,
}

/// Prove a fill ring on a CLONE before any transaction opens — THE "would this face actually
/// weld?" test for every Fill path in this file. Read-only in `mesh`, so every refusal leaves
/// the live mesh byte-identical (`transaction()` always pushes; a refused fill must not eat
/// the user's Ctrl+Z).
///
/// **`create_face` alone proves almost nothing.** `create_face_no_normals`
/// (`core_math/src/half_edge/euler.rs:478`) returns `None` ONLY when the ring has fewer than 3
/// vertices; after that it unconditionally allocates and returns `Some`. Its twin pass
/// (`euler.rs:507-517`) is best-effort — when `find_half_edge(dst, src)` misses it silently
/// leaves `twin == NO_TWIN`. So a ring can "succeed" and produce a face welded to NOTHING, which
/// is exactly what happens at hole corners, where `delete_face` can leave the disk pointer aimed
/// at the rim half-edge and `find_half_edge`'s fan therefore misses real edges
/// (`QUAD_FILL_SINGLE_VERTEX_PLAN.md` §5b findings 1 and 2). `validate()` and `is_manifold()`
/// are what actually catch it: `is_manifold`'s check (a) reports a directed edge used by two
/// faces, which is the signature of a ring wound the same way as the rim it was meant to weld to.
///
/// **A check is only held against the RESULT when the ORIGINAL passed it.** Repairing bad
/// geometry is precisely when a user reaches for Fill, so refusing on a body that was already
/// non-manifold (or already structurally broken) would lock them out of the repair — the same
/// principle behind `integrity()` being split out of `is_manifold()` for edit-op gates
/// (`core_math/src/half_edge/validate.rs:150`, owner directive 2026-08-03: "all tools must work
/// even on open / non-manifold shapes"). Both original-side calls are short-circuited behind the
/// scratch-side failure, so a clean mesh pays for exactly one `validate` + one `is_manifold`.
fn prove_weld(mesh: &HalfEdgeMesh, ring: &[VertexId]) -> WeldProof {
    prove_weld_staged(mesh, |_| ring.to_vec())
}

/// [`prove_weld`] for a ring that does not exist yet.
///
/// Extend mode's fourth corner is INVENTED, so its ring cannot be named until the vertex has been
/// allocated — and allocating it on the live mesh before the proof would mutate a mesh the proof
/// may be about to refuse. `stage` therefore runs on the CLONE and returns the ring it just made
/// namable. Everything after that is the one shared verdict above, which is the point: there is
/// no second "would this face weld?" implementation, only a second way of naming the ring.
fn prove_weld_staged(
    mesh: &HalfEdgeMesh,
    stage: impl FnOnce(&mut HalfEdgeMesh) -> Vec<VertexId>,
) -> WeldProof {
    let mut scratch = mesh.clone();
    let ring = stage(&mut scratch);
    let Some(face) = scratch.create_face(&ring) else { return WeldProof::Failed };
    // NOTE the asymmetry, and keep it: the scratch is the STAGED mesh, but the original-side
    // "did it already fail this?" questions are asked of `mesh`, which staging never touched.
    if scratch.validate().is_err() && mesh.validate().is_ok() {
        return WeldProof::Unsafe;
    }
    if scratch.is_manifold().is_err() && mesh.is_manifold().is_ok() {
        return WeldProof::Unsafe;
    }
    WeldProof::Ok { scratch, face }
}

/// Does `f` fold back OVER the surface it welded to, instead of closing a gap in it?
///
/// A hole rim and the OUTER rim of an open sheet are topologically identical — nothing in the
/// half-edge structure tells them apart (`QUAD_FILL_SINGLE_VERTEX_PLAN.md` §5b). So closing a
/// "gap" at an outer-rim vertex lays a face ON TOP of the sheet instead of filling anything, and
/// it welds perfectly while doing so: `prove_weld` cannot see it, because nothing topological is
/// wrong.
///
/// The test is geometric. Across each edge `f` shares with an existing face, `f`'s centroid must
/// lie on the FAR side of that edge from the neighbour's centroid. Both offsets are measured
/// PERPENDICULAR to the shared edge, so curvature along the edge cannot flip the sign. Same side
/// (a positive dot) = the new face lies back over its neighbour.
///
/// **This is a WARNING, never a refusal, and must not be "upgraded" into one.** It
/// false-negatives on a CONCAVE rim, where a legitimate hole-closing face genuinely does sit on
/// the same side of a shared edge as its neighbour; refusing on that signal would block correct
/// fills on any non-convex hole. The owner's decision (2026-08-09) is: fill it, and toast so the
/// result is visible and one Ctrl+Z away.
fn face_folds_over_neighbour(mesh: &HalfEdgeMesh, f: FaceId) -> bool {
    if !mesh.is_face_live(f) { return false; }
    let ring = mesh.face_vertices(f);
    if ring.len() < 3 { return false; }
    let mine_c = centroid(&ring.iter().map(|&v| mesh.vert(v).position).collect::<Vec<_>>());

    let mut nb_ring: Vec<VertexId> = Vec::new();
    for h in mesh.face_half_edges(f) {
        let twin = mesh.he(h).twin;
        if twin == NO_TWIN || !mesh.is_edge_live(twin) { continue; }
        let nf = mesh.he(twin).face;
        if nf == NO_FACE || nf == f || !mesh.is_face_live(nf) { continue; }

        let a = mesh.vert(mesh.he_source(h)).position;
        let b = mesh.vert(mesh.he(h).vertex).position;
        let dir = (b - a).normalize_or_zero();
        if dir.length_squared() < 0.5 { continue; } // zero-length edge — no side to be on
        let mid = (a + b) * 0.5;

        mesh.face_vertices_into(nf, &mut nb_ring);
        if nb_ring.len() < 3 { continue; }
        let their_c = centroid(&nb_ring.iter().map(|&v| mesh.vert(v).position).collect::<Vec<_>>());

        let across = |p: Vec3| {
            let d = p - mid;
            (d - dir * d.dot(dir)).normalize_or_zero()
        };
        let mine = across(mine_c);
        let theirs = across(their_c);
        if mine.length_squared() < 0.5 || theirs.length_squared() < 0.5 { continue; }
        // Strictly positive with a margin: a reading near zero means the centroid sits on the
        // edge line itself, which is degenerate rather than evidence of a fold.
        if mine.dot(theirs) > 1e-3 { return true; }
    }
    false
}

/// Plain arithmetic mean of a point set. `core_math`'s `face_center_median_weighted` is Blender's
/// EDGE-WEIGHTED poke centre and deliberately a different point, so it is not the right tool for a
/// side-of-edge test; this is the same mean `try_fill_ring` uses for its angular sort.
fn centroid(pts: &[Vec3]) -> Vec3 {
    if pts.is_empty() { return Vec3::ZERO; }
    pts.iter().copied().sum::<Vec3>() / pts.len() as f32
}

enum FillTry {
    Ok(Vec<VertexId>),
    Degenerate,
    /// Neither winding welds soundly — the face would install onto a mesh it breaks
    /// (a duplicated directed edge, i.e. a face attached to nothing). See [`prove_weld`].
    Unsafe,
    Failed,
}

/// Build the ordered fill ring, or refuse. Does not mutate the mesh (proves `create_face` on a
/// clone via [`prove_weld`]).
///
/// **Winding is topological first, camera-facing only as a tiebreak.** A rim half-edge runs in
/// its adjacent face's direction, so a face filling that rim MUST wind opposite it or
/// `create_face`'s twin lookup finds nothing and the face welds to NOTHING
/// (`euler.rs:510`; same rule `cap_boundary_loops` states). The camera-facing flip below is a
/// coin flip against that requirement — from "below" a hole it produced exactly the broken face
/// `prove_weld` now catches. Rather than turn half the camera angles into a refusal (which is
/// what adding the guard alone would do), a refused ring is retried REVERSED: topology decides
/// when the ring has neighbours to weld to, and the camera choice still stands for a free-floating
/// ring, where both windings weld equally and only the normal's direction is at stake.
///
/// LIMIT, stated rather than hidden: on an ALREADY non-manifold body the manifold gate is
/// disabled by design (see [`prove_weld`]), so the first winding is always accepted and the retry
/// cannot fire there. That is exactly the behaviour this path had before the guard existed — no
/// regression — but it does mean a dirty body can still take a badly-wound fill. Closing it needs
/// a signal that survives a dirty mesh (counting welded rim edges per winding, say), which is a
/// larger change than this task.
fn try_fill_ring(mesh: &HalfEdgeMesh, verts: &[u32], cam_forward: Vec3) -> FillTry {
    let pts: Vec<(VertexId, Vec3)> = verts.iter()
        .map(|&v| (VertexId(v), mesh.vert(VertexId(v)).position))
        .collect();
    let positions: Vec<Vec3> = pts.iter().map(|&(_, p)| p).collect();

    let plane_n = best_fit_plane_normal(&positions);
    if plane_n.length_squared() < 1e-10 {
        return FillTry::Degenerate;
    }
    let centroid = centroid(&positions);

    let mut u = positions[0] - centroid;
    u = (u - plane_n * u.dot(plane_n)).normalize_or_zero();
    if u.length_squared() < 1e-10 {
        let seed = if plane_n.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
        u = (seed - plane_n * seed.dot(plane_n)).normalize_or_zero();
    }
    let v_axis = plane_n.cross(u);

    let mut ordered: Vec<(VertexId, f32)> = pts.iter().map(|&(vid, p)| {
        let rel = p - centroid;
        (vid, rel.dot(v_axis).atan2(rel.dot(u)))
    }).collect();
    ordered.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut ring: Vec<VertexId> = ordered.iter().map(|&(vid, _)| vid).collect();

    let ring_pts: Vec<Vec3> = ring.iter().map(|&vid| mesh.vert(vid).position).collect();
    let ring_n = best_fit_plane_normal(&ring_pts);
    if ring_n.dot(-cam_forward) < 0.0 {
        ring.reverse();
    }

    // Prove the ring actually WELDS, without mutating the live mesh. `create_face` succeeding is
    // not proof of anything but the length check — see `prove_weld`.
    match prove_weld(mesh, &ring) {
        WeldProof::Ok { .. } => return FillTry::Ok(ring),
        // A ring too short for `create_face` stays too short reversed — no point retrying.
        WeldProof::Failed => return FillTry::Failed,
        WeldProof::Unsafe => {}
    }
    // The camera-facing flip guessed wrong about a rim that has only one correct winding.
    // Topology wins (see this function's doc comment).
    ring.reverse();
    match prove_weld(mesh, &ring) {
        WeldProof::Ok { .. } => FillTry::Ok(ring),
        WeldProof::Failed => FillTry::Failed,
        WeldProof::Unsafe => FillTry::Unsafe,
    }
}

/// Fill gates — the weld guard (Task 1), the outer-rim warning (Task 2) and extend mode (P4).
///
/// These test the pure planners, which is where every accept/refuse decision is made:
/// `poly_fill_face` / `poly_quad_fill_at_vertex` only toast and open the transaction around what
/// these return. The fixtures are local rather than shared with `nav.rs`'s (that module's test
/// helpers are `#[cfg(test)]`-private to `core_math` and cannot be reached from `app`).
#[cfg(test)]
mod tests {
    use super::*;

    /// A 5x5 quad plane with the CENTRE quad deleted: a 4-corner hole with grid left over on
    /// every side. Returns the mesh and the hole's 4 corners.
    fn plane_with_square_hole() -> (HalfEdgeMesh, Vec<VertexId>) {
        let mut m = HalfEdgeMesh::new_plane(5.0, 5.0, 5, 5);
        let centre = interior_face(&m, &[]).expect("an interior quad");
        let corners = m.face_vertices(centre);
        assert_eq!(corners.len(), 4);
        m.delete_face(centre);
        (m, corners)
    }

    /// A live face whose every edge is twinned (so it touches no rim) and none of whose corners
    /// is in `avoid`. Deterministic: lowest face id wins.
    fn interior_face(m: &HalfEdgeMesh, avoid: &[VertexId]) -> Option<FaceId> {
        (0..m.faces.len() as u32).map(FaceId).find(|&f| {
            m.is_face_live(f)
                && m.face_vertices(f).len() == 4
                && m.face_half_edges(f).iter().all(|&h| m.he(h).twin != NO_TWIN)
                && m.face_vertices(f).iter().all(|v| !avoid.contains(v))
        })
    }

    /// Live half-edges with no twin — i.e. how much open rim the mesh still has. A fill that
    /// really welded consumes exactly one per shared edge.
    fn rim_count(m: &HalfEdgeMesh) -> usize {
        (0..m.half_edges.len() as u32)
            .map(HalfEdgeId)
            .filter(|&h| m.is_edge_live(h) && m.he(h).twin == NO_TWIN)
            .count()
    }

    fn signature(m: &HalfEdgeMesh) -> (u64, u64, usize, usize, usize) {
        (m.content_signature(), m.topology_signature(), m.vertex_count(), m.face_count(), m.edge_count())
    }

    /// TASK 1 REGRESSION GUARD. The weld guard must not turn a valid fill into a refusal —
    /// including from the camera side where the old camera-facing winding guessed wrong (which
    /// is exactly the case the guard would otherwise start rejecting). Both sides must fill,
    /// and the result must actually weld: 4 rim half-edges consumed, manifold intact.
    #[test]
    fn a_clean_hole_still_fills_from_either_camera_side() {
        for cam in [Vec3::new(0.0, -1.0, 0.0), Vec3::new(0.0, 1.0, 0.0)] {
            let (m, corners) = plane_with_square_hole();
            let ids: Vec<u32> = corners.iter().map(|v| v.0).collect();
            let rim_before = rim_count(&m);

            let ring = match try_fill_ring(&m, &ids, cam) {
                FillTry::Ok(r) => r,
                FillTry::Degenerate => panic!("cam {cam:?}: refused a clean hole as degenerate"),
                FillTry::Unsafe => panic!("cam {cam:?}: refused a clean hole as unsafe — the guard tightened a VALID fill"),
                FillTry::Failed => panic!("cam {cam:?}: refused a clean hole"),
            };

            let mut out = m.clone();
            let fid = out.create_face(&ring).expect("the proven ring must apply");
            assert_eq!(out.face_vertices(fid).len(), 4, "cam {cam:?}: must be a quad");
            assert!(out.validate().is_ok(), "cam {cam:?}: {:?}", out.validate());
            assert!(out.is_manifold().is_ok(), "cam {cam:?}: {:?}", out.is_manifold());
            assert_eq!(
                rim_count(&out), rim_before - 4,
                "cam {cam:?}: the fill must CONSUME the hole's 4 rim half-edges, not float beside them"
            );
        }
    }

    /// A ring that `create_face` accepts but that welds to nothing (stamping a second face over
    /// an existing one duplicates a directed edge in BOTH windings) must be refused — and the
    /// refusal must leave the mesh untouched, because `transaction()` has not opened yet.
    #[test]
    fn a_ring_that_welds_to_nothing_is_refused_and_the_mesh_is_untouched() {
        let m = HalfEdgeMesh::new_plane(5.0, 5.0, 5, 5);
        let f = interior_face(&m, &[]).expect("an interior quad");
        let ids: Vec<u32> = m.face_vertices(f).iter().map(|v| v.0).collect();
        let before = signature(&m);

        match try_fill_ring(&m, &ids, Vec3::new(0.0, -1.0, 0.0)) {
            FillTry::Unsafe => {}
            FillTry::Ok(_) => panic!("a face stamped over an existing one welds to NOTHING — it must be refused"),
            other => panic!("expected Unsafe, got {}", match other {
                FillTry::Degenerate => "Degenerate", FillTry::Failed => "Failed", _ => "?",
            }),
        }
        assert_eq!(signature(&m), before, "a refusal must not mutate the mesh");
    }

    /// The manifold gate must never lock a user out of REPAIRING bad geometry: an already
    /// non-manifold body still fills. (Same principle as `integrity()` being split out of
    /// `is_manifold()` for edit-op gates.)
    #[test]
    fn an_already_non_manifold_body_can_still_be_filled() {
        let (mut m, corners) = plane_with_square_hole();
        // Dirty the body FAR from the hole: stamp a duplicate face over an interior quad. Twin
        // symmetry survives (validate stays green); edge multiplicity does not.
        let junk = interior_face(&m, &corners).expect("an interior quad away from the hole");
        let junk_ring = m.face_vertices(junk);
        m.create_face(&junk_ring).expect("stamp");
        assert!(m.validate().is_ok(), "fixture must stay structurally valid: {:?}", m.validate());
        assert!(m.is_manifold().is_err(), "fixture must be non-manifold to be worth testing");

        let ids: Vec<u32> = corners.iter().map(|v| v.0).collect();
        assert!(
            matches!(try_fill_ring(&m, &ids, Vec3::new(0.0, -1.0, 0.0)), FillTry::Ok(_)),
            "a non-manifold body must stay editable"
        );
    }

    /// TASK 2 (both halves — the detector is untested if only one fires). Closing a real hole
    /// must NOT warn; closing at the sheet's own outer rim must fill AND warn.
    #[test]
    fn outer_rim_quad_fill_warns_and_a_real_hole_does_not() {
        let (m, corners) = plane_with_square_hole();
        match try_closing_ring(&m, corners[0]) {
            ClosingTry::Ok { ring, over_surface } => {
                assert_eq!(ring.len(), 4, "a 4-corner hole closes with a quad");
                assert!(!over_surface, "closing a REAL hole must not warn");
            }
            _ => panic!("a hole corner must close"),
        }

        // The outer rim of an open sheet is topologically identical to a hole rim, so this
        // fills — and lays a quad over the sheet's corner while doing so.
        let sheet = HalfEdgeMesh::new_plane(4.0, 4.0, 2, 2);
        let corner = (0..sheet.vertices.len() as u32)
            .map(VertexId)
            .find(|&v| {
                sheet.is_vert_live(v) && {
                    let p = sheet.vert(v).position;
                    (p.x + 2.0).abs() < 1e-4 && (p.z + 2.0).abs() < 1e-4
                }
            })
            .expect("the sheet's (-2,0,-2) corner");
        match try_closing_ring(&sheet, corner) {
            ClosingTry::Ok { over_surface, .. } => assert!(
                over_surface,
                "an outer-rim fill lies OVER the surface and must warn (owner: warn, never refuse)"
            ),
            _ => panic!("the outer-rim case must still FILL — the owner chose warn, not refuse"),
        }
    }

    // ========================================================
    // P4 — extend mode (`QUAD_FILL_SINGLE_VERTEX_PLAN.md` §3 P4)
    // ========================================================

    /// Build a mesh from an explicit point list and face rings — the fixtures below need
    /// shared-vertex control that `new_plane` cannot give (a missing corner cell, two quads
    /// touching at one point). Same `alloc_vertex` + `create_face` shape `bridgediag.rs` uses.
    fn mesh_from_faces(pts: &[Vec3], faces: &[&[usize]]) -> (HalfEdgeMesh, Vec<VertexId>) {
        let mut m = HalfEdgeMesh::new();
        let vs: Vec<VertexId> = pts
            .iter()
            .map(|&p| {
                m.alloc_vertex(HVertex { position: p, normal: Vec3::Y, half_edge: NO_EDGE })
            })
            .collect();
        for f in faces {
            let ring: Vec<VertexId> = f.iter().map(|&i| vs[i]).collect();
            m.create_face(&ring).expect("fixture face");
        }
        assert!(m.validate().is_ok(), "fixture must be valid: {:?}", m.validate());
        (m, vs)
    }

    /// **The owner's shape.** Three quads of a 2x2 grid; the fourth cell is missing and its far
    /// corner `(2, ·, 2)` exists nowhere in the mesh. Vertex 4 is the notch corner: the border
    /// leaves it on two sides and no walk of that border can reach a 4th corner for the gap.
    ///
    /// `lift` raises the outer column and row in Y, bending the border off the flat plane — that
    /// is the CURVED fixture the projection's non-vacuity gate needs.
    fn l_notch(lift: f32) -> (HalfEdgeMesh, Vec<VertexId>) {
        mesh_from_faces(
            &[
                Vec3::new(0.0, 0.0, 0.0),  // 0
                Vec3::new(1.0, 0.0, 0.0),  // 1
                Vec3::new(2.0, lift, 0.0), // 2
                Vec3::new(0.0, 0.0, 1.0),  // 3
                Vec3::new(1.0, 0.0, 1.0),  // 4  <- the selected vertex
                Vec3::new(2.0, lift, 1.0), // 5
                Vec3::new(0.0, lift, 2.0), // 6
                Vec3::new(1.0, lift, 2.0), // 7
            ],
            &[&[0, 1, 4, 3], &[1, 2, 5, 4], &[3, 4, 7, 6]],
        )
    }

    /// THE P4 GATE. On an open border corner whose 4th corner does not exist, `F` adds exactly
    /// ONE vertex and ONE quad — and that quad WELDS to the border rather than floating beside it.
    #[test]
    fn extend_grows_exactly_one_vertex_and_one_quad_at_a_border_corner() {
        let (m, vs) = l_notch(0.0);
        let v = vs[4];
        let ExtendTry::Ok { plan, over_surface } = try_extend_quad(&m, v) else {
            panic!("a border corner with no 4th corner is exactly extend mode's case");
        };
        assert!(!over_surface, "the notch is a real gap — the quad fills it, it does not lie over it");
        assert_eq!(
            (plan.a, plan.v, plan.e),
            (vs[5], vs[4], vs[7]),
            "the three EXISTING corners are the selected vertex and its two boundary neighbours"
        );
        // X = A + E - V, and here that is the missing cell's far corner.
        assert!(
            (plan.x - Vec3::new(2.0, 0.0, 2.0)).length() < 1e-5,
            "the parallelogram completion must land on the missing corner, got {:?}",
            plan.x
        );

        let (outs, ins) = rim_edges_at(&m, v);
        let before = signature(&m);
        let rim_before = rim_count(&m);

        let mut out = m.clone();
        let ring = stage_extend_quad(&mut out, &plan);
        let fid = out.create_face(&ring).expect("the proven ring must apply");

        assert_eq!(out.vertex_count(), before.2 + 1, "exactly ONE invented vertex");
        assert_eq!(out.face_count(), before.3 + 1, "exactly ONE new face");
        assert_eq!(out.face_vertices(fid).len(), 4, "and it must be a QUAD");
        assert!(out.validate().is_ok(), "{:?}", out.validate());
        assert!(out.is_manifold().is_ok(), "{:?}", out.is_manifold());
        assert!(
            out.he(ins[0]).twin != NO_TWIN && out.he(outs[0]).twin != NO_TWIN,
            "the grown quad must WELD to the two rim half-edges it grew from"
        );
        assert_eq!(
            rim_count(&out),
            rim_before,
            "2 rim half-edges consumed, 2 new ones added — a floating quad would add 4"
        );
    }

    /// The grown quad continues the surface: every corner, invented one included, lies in the
    /// plane of the face(s) it grew from.
    #[test]
    fn the_grown_quad_stays_coplanar_with_the_surface_it_grew_from() {
        let (m, vs) = l_notch(0.0);
        let ExtendTry::Ok { plan, .. } = try_extend_quad(&m, vs[4]) else { panic!("must grow") };
        let (outs, ins) = rim_edges_at(&m, vs[4]);
        let n = surface_normal_at_rim(&m, ins[0], outs[0]);
        let anchor = m.vert(vs[4]).position;
        for p in [
            m.vert(plan.a).position,
            m.vert(plan.v).position,
            m.vert(plan.e).position,
            plan.x,
        ] {
            assert!((p - anchor).dot(n).abs() < 1e-5, "corner {p:?} left the surface plane");
        }
    }

    /// NON-VACUITY. Without this the projection could be a no-op and nothing would notice: on a
    /// CURVED border the projected corner must actually MOVE off the raw `A + E − V` point — and
    /// land ON the plane — while on a flat border it must NOT move (which is what proves the gate
    /// is measuring curvature and not some constant offset).
    #[test]
    fn on_a_curved_border_the_projection_moves_the_invented_corner_off_the_parallelogram() {
        let (m, vs) = l_notch(0.5);
        let ExtendTry::Ok { plan, .. } = try_extend_quad(&m, vs[4]) else { panic!("must grow") };
        // The raw rule, recomputed here independently of the implementation.
        let raw = m.vert(plan.a).position + m.vert(plan.e).position - m.vert(plan.v).position;
        let moved = (plan.x - raw).length();
        assert!(moved > 1e-3, "the projection is a NO-OP on a curved border (moved {moved})");

        let (outs, ins) = rim_edges_at(&m, vs[4]);
        let n = surface_normal_at_rim(&m, ins[0], outs[0]);
        let off = (plan.x - m.vert(plan.v).position).dot(n).abs();
        assert!(off < 1e-5, "the projected corner must LIE on the plane, off by {off}");

        let (flat, fvs) = l_notch(0.0);
        let ExtendTry::Ok { plan: fp, .. } = try_extend_quad(&flat, fvs[4]) else { panic!() };
        let fraw = flat.vert(fp.a).position + flat.vert(fp.e).position - flat.vert(fp.v).position;
        assert!(
            (fp.x - fraw).length() < 1e-6,
            "on a FLAT border the parallelogram point is already on the surface"
        );
    }

    /// UNDO. `transaction()` snapshots the whole scene graph (`undo.rs:809`), so ONE Ctrl+Z
    /// restores this mesh wholesale — what has to hold for that to be ENOUGH is that the quad and
    /// its invented corner are one mesh mutation with nothing outside the snapshot.
    #[test]
    fn one_undo_removes_both_the_grown_quad_and_the_invented_corner() {
        let (m, vs) = l_notch(0.0);
        let ExtendTry::Ok { plan, .. } = try_extend_quad(&m, vs[4]) else { panic!("must grow") };
        let before = signature(&m);
        let snapshot = m.clone(); // what `transaction()` stores before the closure runs

        let mut live = m;
        let ring = stage_extend_quad(&mut live, &plan);
        let x = *ring.last().expect("the invented corner is the ring's last vertex");
        live.create_face(&ring).expect("apply");
        assert_eq!(live.vertex_count(), before.2 + 1);
        assert_eq!(live.face_count(), before.3 + 1);
        assert!(live.is_vert_live(x));
        assert!(
            !snapshot.is_vert_live(x),
            "the invented corner must not exist in the pre-press mesh, or undo would keep it"
        );
        assert_eq!(
            signature(&snapshot),
            before,
            "restoring that ONE snapshot takes the face AND the vertex away together"
        );
    }

    /// The outer-rim warning fires in EXTEND mode too, not just closing. A lone triangle has no
    /// gap at all — its border is its own outer rim — so the parallelogram completion lays a quad
    /// back over it. It still welds, and owner 2026-08-09 is warn-never-refuse, so the flag has to
    /// be raised rather than the fill blocked.
    #[test]
    fn the_outer_rim_warning_fires_in_extend_mode_too() {
        let (m, vs) = mesh_from_faces(
            &[Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0)],
            &[&[0, 1, 2]],
        );
        match try_extend_quad(&m, vs[0]) {
            ExtendTry::Ok { over_surface, .. } => assert!(
                over_surface,
                "a quad grown over the sheet's own rim must WARN (owner: warn, never refuse)"
            ),
            _ => panic!("the outer-rim case must still GROW — the owner chose warn, not refuse"),
        }
    }

    /// A pinch vertex has SEVERAL boundary pairs, so "the two boundary neighbours" is ambiguous
    /// and `boundary_edges_at` would answer for whichever sector the disk pointer happens to name
    /// (plan §5b finding 3). Decline explicitly instead of picking one and pretending.
    #[test]
    fn two_borders_meeting_at_one_vertex_decline_instead_of_picking_a_sector() {
        // Two quads touching at exactly one vertex (index 2).
        let (m, vs) = mesh_from_faces(
            &[
                Vec3::new(0.0, 0.0, 0.0), // 0
                Vec3::new(1.0, 0.0, 0.0), // 1
                Vec3::new(1.0, 0.0, 1.0), // 2  <- the pinch
                Vec3::new(0.0, 0.0, 1.0), // 3
                Vec3::new(2.0, 0.0, 1.0), // 4
                Vec3::new(2.0, 0.0, 2.0), // 5
                Vec3::new(1.0, 0.0, 2.0), // 6
            ],
            &[&[0, 1, 2, 3], &[2, 4, 5, 6]],
        );
        let (outs, ins) = rim_edges_at(&m, vs[2]);
        assert_eq!((outs.len(), ins.len()), (2, 2), "fixture must really be a pinch");
        let before = signature(&m);
        assert!(matches!(try_extend_quad(&m, vs[2]), ExtendTry::Pinch));
        assert_eq!(signature(&m), before, "a refusal must not mutate the mesh");
    }

    /// A straight border gives `A + E − V == V`, i.e. a zero-width quad. It must decline — and
    /// the decline the USER sees must be EXTEND's, which is what proves the fall-through from
    /// closing actually ran.
    #[test]
    fn a_straight_border_falls_through_to_extend_and_declines_there() {
        let m = HalfEdgeMesh::new_plane(10.0, 10.0, 10, 10);
        let v = (0..m.vertices.len() as u32)
            .map(VertexId)
            .find(|&v| {
                m.is_vert_live(v) && {
                    let p = m.vert(v).position;
                    p.x.abs() < 1e-4 && (p.z + 5.0).abs() < 1e-4
                }
            })
            .expect("the middle vertex of the sheet's -Z rim");
        let before = signature(&m);
        assert!(
            matches!(try_closing_ring(&m, v), ClosingTry::Degenerate),
            "4 collinear rim corners are not a gap"
        );
        assert_eq!(plan_quad_fill(&m, v).err(), Some(QuadFillDecline::Straight));
        assert_eq!(signature(&m), before, "a refusal must not mutate the mesh");
    }

    /// ROUTING. The owner's L-notch must reach EXTEND through the dispatcher, not just when
    /// `try_extend_quad` is called directly — the rim walk there leaves the notch and closes
    /// across an already-interior edge, which `try_closing_ring` reports as `Unsafe`.
    #[test]
    fn the_l_notch_routes_to_extend_through_the_dispatcher() {
        let (m, vs) = l_notch(0.0);
        assert!(
            matches!(try_closing_ring(&m, vs[4]), ClosingTry::Unsafe),
            "closing must refuse the notch — its walked ring closes over an interior edge"
        );
        match plan_quad_fill(&m, vs[4]) {
            Ok(QuadFillPlan::Extend { over_surface, .. }) => assert!(!over_surface),
            Ok(QuadFillPlan::Closing { .. }) => panic!("the notch has no 4th corner to close onto"),
            Err(d) => panic!("the notch must GROW a quad, not decline with {d:?}"),
        }
    }

    /// CLOSING REGRESSION. A real 4-corner hole still fills with the corners that already exist —
    /// extend must not have stolen the case, and closing must not have started inventing.
    #[test]
    fn closing_mode_still_fills_a_real_hole_without_inventing_a_vertex() {
        let (m, corners) = plane_with_square_hole();
        let before = signature(&m);
        let Ok(QuadFillPlan::Closing { ring, over_surface }) = plan_quad_fill(&m, corners[0]) else {
            panic!("a real 4-corner hole must CLOSE, never grow");
        };
        assert_eq!(ring.len(), 4);
        assert!(!over_surface, "closing a REAL hole must not warn");
        assert!(
            ring.iter().all(|c| corners.contains(c)),
            "every corner of the filling quad must already exist on the rim"
        );
        let mut out = m.clone();
        out.create_face(&ring).expect("apply");
        assert_eq!(out.vertex_count(), before.2, "closing must invent NOTHING");
        assert_eq!(out.face_count(), before.3 + 1);
    }

    /// An interior vertex has no border to close: decline, mesh untouched, no undo entry burned.
    #[test]
    fn an_interior_vertex_declines_without_touching_the_mesh() {
        let (m, corners) = plane_with_square_hole();
        let interior = (0..m.vertices.len() as u32)
            .map(VertexId)
            .find(|&v| m.is_vert_live(v) && !corners.contains(&v) && !m.vertex_is_boundary(v))
            .expect("an interior grid vertex");
        let before = signature(&m);
        assert!(matches!(try_closing_ring(&m, interior), ClosingTry::Interior));
        assert_eq!(signature(&m), before);
    }
}

/// Best-fit plane normal for a point set. Newell's method over the given order,
/// falling back to the first non-collinear triple when Newell degenerates (e.g.
/// a scrambled bowtie order). Returns a zero vector only when fully collinear.
fn best_fit_plane_normal(positions: &[Vec3]) -> Vec3 {
    let n = positions.len();
    if n < 3 { return Vec3::ZERO; }

    let mut normal = Vec3::ZERO;
    for i in 0..n {
        let cur = positions[i];
        let next = positions[(i + 1) % n];
        normal.x += (cur.y - next.y) * (cur.z + next.z);
        normal.y += (cur.z - next.z) * (cur.x + next.x);
        normal.z += (cur.x - next.x) * (cur.y + next.y);
    }
    if normal.length_squared() > 1e-12 {
        return normal.normalize();
    }
    // Fallback: first non-collinear triple.
    for i in 0..n {
        for j in (i + 1)..n {
            for k in (j + 1)..n {
                let nrm = (positions[j] - positions[i]).cross(positions[k] - positions[i]);
                if nrm.length_squared() > 1e-12 {
                    return nrm.normalize();
                }
            }
        }
    }
    Vec3::ZERO
}
