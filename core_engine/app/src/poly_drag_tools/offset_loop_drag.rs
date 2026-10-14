// offset_loop_drag.rs — Offset Edge Loop modal (`I` in Edge mode).
//
// Plan: `docs/planning/poly/OFFSET_EDGE_LOOP_PLAN.md` §4 **P4**. Maya's *Offset Edge Loop Tool* /
// Blender's *Offset Edge Loop Cut*: a parallel copy of a selected edge loop — closed, or an OPEN
// chain with two free endpoints (P2, plan §3.3) — the ORIGINAL LOOP KEPT, connecting quads between
// the two. NOT a bevel (bevel consumes the edge) and NOT loop cut (different axis, seeded by a
// ring) — plan §5 keeps the three distinct.
//
// The kernel is READ-ONLY here: `HalfEdgeMesh::offset_edge_loop` (`half_edge/ops_inset.rs`) owns
// every piece of the offset/rail math. This file is pure app wiring — probe, drag→parameter
// mapping, install, undo episode.
//
// ── P3 ──
// Shift = rail BOTH adjacent faces of the selected segment (same |distance|).
// Alt (proportional 0..1) is still inert — not advertised.

use std::collections::HashSet;
use std::sync::Arc;

use eframe::egui;
use glam::{Mat4, Vec3};

use core_math::half_edge::{HalfEdgeId, HalfEdgeMesh, VertexId};
use core_math::scene_graph::{NodeId, NodePayload};

use crate::{ActiveTool, AzraApp, DirtyFlags, PolyOffsetLoopDrag, world_to_screen};

/// Probe offset, as a fraction of the selected loop's mean edge length.
///
/// Only the DIRECTION of the probe result is consumed, and the kernel places each new vertex at
/// `V + d · normalize(rail − V)` — a pure scaling — so the direction is independent of `d` for any
/// `d` below the per-vertex rail clamp. A small fraction keeps every vertex under that clamp on any
/// sane quad mesh, which is what makes the probe direction exact rather than merely indicative.
const PROBE_FRACTION: f32 = 0.05;

/// Pixels → world offset. Same constant and same camera-distance scaling as the bevel modal
/// (`poly_update_bevel_drag`), so an absolute-width drag feels identical across the two tools.
const OFFSET_DRAG_SCALE: f32 = 0.002;

/// Run the P1 kernel ONCE on a throwaway clone and report **where the new loop actually lands**, as
/// a world-space centroid — or the kernel's own refusal reason.
///
/// This function is the entire answer to plan §7b's warning: `distance`'s sign selects the side by
/// reversing the ring, and the ring's orientation falls out of `edge_loop_rings`' lowest-vertex-id
/// start, so `+` is deterministic but **geometrically arbitrary**. A tool that assumed `+` meant
/// "the side the user dragged toward" would flip sides depending on which vertex happens to hold
/// the lowest id. Instead both signs are run here at arm time and the drag is bound to whichever
/// one demonstrably moves the loop toward the cursor.
///
/// Probing the kernel DIRECTLY (rather than `OffsetEdgeLoopOp` via the `MeshOp` trait) is
/// deliberate — plan §7b: the wrapper pays a mesh clone even on refusal, and the kernel is atomic
/// on refusal by design, so this is both cheaper and the documented probe path.
fn probe_landing(
    base: &HalfEdgeMesh,
    sel: &HashSet<HalfEdgeId>,
    signed: f32,
    wm: Mat4,
) -> Result<Vec3, String> {
    let mut m = base.clone();
    let new_verts = m.offset_edge_loop(sel, signed)?;
    if new_verts.is_empty() {
        return Err("offset produced no vertices".to_string());
    }
    let mut c = Vec3::ZERO;
    for v in &new_verts {
        c += m.vert(*v).position;
    }
    // Mesh positions are BODY-LOCAL (CLAUDE.md § Prim Position Space). The caller's `wm` is the
    // node's world matrix, applied here so every screen projection downstream is honest.
    Ok(wm.transform_point3(c / new_verts.len() as f32))
}

impl AzraApp {
    /// Project a world point to viewport screen space, using the same camera idiom as the other
    /// poly drag tools (`poly/slide.rs`, `poly/loop_cut.rs`).
    fn offset_loop_to_screen(&self, w: Vec3, rect: egui::Rect) -> Option<egui::Pos2> {
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        world_to_screen(
            w,
            cam_pos,
            self.viewport.camera.orbit,
            rect,
            aspect,
            ortho_hw,
            self.render.fov_deg,
        )
    }

    /// Screen-space unit axis pointing from the original loop toward where a given side lands.
    /// `None` when that side is illegal, or when the view is edge-on to the offset direction (the
    /// projected axis collapses and cannot honestly discriminate a drag direction).
    ///
    /// The world direction is walked a CAMERA-RELATIVE distance before projecting, so the screen
    /// axis is tens of pixels long at any zoom. Projecting the probe's actual landing point instead
    /// would give a sub-pixel axis (the probe is a few percent of an edge length by design) and the
    /// "is this edge-on?" test could not tell a small offset from a degenerate one.
    fn offset_loop_axis(
        &self,
        anchor_world: Vec3,
        dir_world: Option<Vec3>,
        rect: egui::Rect,
    ) -> Option<egui::Vec2> {
        let dir = dir_world?;
        let step = (self.viewport.camera.distance * 0.05).max(1e-4);
        let a = self.offset_loop_to_screen(anchor_world, rect)?;
        let b = self.offset_loop_to_screen(anchor_world + dir * step, rect)?;
        let v = b - a;
        if v.length() < 0.5 { None } else { Some(v.normalized()) }
    }

    /// Arm the Offset Edge Loop modal. `I` in Edge mode with a non-empty edge selection.
    ///
    /// Refusals here are FAIL-CLOSED: the probe runs on throwaway clones, so nothing is mutated,
    /// and the undo episode is opened only AFTER a legal side is proven — a refusal can never push
    /// an undo entry.
    pub(crate) fn poly_activate_offset_edge_loop(&mut self) {
        // Re-press while live = confirm, same shape as `poly_activate_bevel`.
        if self.tool.current.is_poly_offset_loop() {
            self.poly_confirm_offset_loop();
            return;
        }
        let Some(nid) = self.sel.usel.active_node() else {
            self.toast("Offset Edge Loop: select a poly object first");
            return;
        };
        let Some(original_mesh) = self.poly_node_mesh(nid) else {
            self.toast("Offset Edge Loop: active object is not a poly mesh");
            return;
        };

        // Drop selection ids the mesh no longer carries (stale after another topology op).
        let n_he = original_mesh.half_edges.len() as u32;
        let edges: HashSet<u32> = self
            .poly_sel
            .selected_edges
            .iter()
            .copied()
            .filter(|&e| e < n_he && original_mesh.is_edge_live(HalfEdgeId(e)))
            .collect();
        if edges.is_empty() {
            // P2 (plan §3.3): an OPEN chain with two free endpoints is legal input — the owner's
            // silhouette selection is one — so this must not tell the user to close the loop.
            self.toast("Offset Edge Loop: select an edge loop or chain first");
            return;
        }
        let sel: HashSet<HalfEdgeId> = edges.iter().map(|&e| HalfEdgeId(e)).collect();

        // Loop centroid + mean edge length, both from the ORIGINAL (body-local) mesh.
        let mut loop_vs: HashSet<u32> = HashSet::new();
        let mut len_sum = 0.0f32;
        for &e in &edges {
            let h = HalfEdgeId(e);
            let src = original_mesh.he_source(h);
            let dst = original_mesh.he(h).vertex;
            loop_vs.insert(src.0);
            loop_vs.insert(dst.0);
            len_sum += (original_mesh.vert(dst).position - original_mesh.vert(src).position).length();
        }
        let probe = (len_sum / edges.len() as f32) * PROBE_FRACTION;
        if !(probe > 0.0) || loop_vs.is_empty() {
            self.toast("Offset Edge Loop: selected edges have zero length");
            return;
        }
        let wm = self.scene.scene_graph.world_transform(nid);
        let anchor_local = loop_vs
            .iter()
            .fold(Vec3::ZERO, |a, &v| a + original_mesh.vert(VertexId(v)).position)
            / loop_vs.len() as f32;
        let anchor_world = wm.transform_point3(anchor_local);

        // ── BIND DRAG DIRECTION TO SIGN BY MEASUREMENT, NEVER BY ASSUMPTION (plan §7b) ──
        // Every refusal the kernel can raise for a *side* is topological (no off-loop rail, boundary
        // edge, ambiguous rail, two loop edges bounding the same face), i.e. distance-independent —
        // the only distance-dependent refusal is the inversion guard, which cannot fire at
        // PROBE_FRACTION of an edge length. So "probe refused" == "this side is illegal", honestly.
        let land_pos = probe_landing(&original_mesh, &sel, probe, wm);
        let land_neg = probe_landing(&original_mesh, &sel, -probe, wm);
        if land_pos.is_err() && land_neg.is_err() {
            let why = land_pos.unwrap_err();
            self.toast(&format!("Offset Edge Loop refused: {why}"));
            return;
        }
        let one_sided = land_pos.is_err() || land_neg.is_err();
        // Landing POINT → unit DIRECTION from the loop. `normalize_or_zero` per CLAUDE.md; a zero
        // here means the probe did not move the loop measurably, which is not a usable axis.
        let to_dir = |r: Result<Vec3, String>| -> Option<Vec3> {
            let d = (r.ok()? - anchor_world).normalize_or_zero();
            if d.length_squared() > 0.5 { Some(d) } else { None }
        };
        let dir_pos_world = to_dir(land_pos);
        let dir_neg_world = to_dir(land_neg);

        self.begin_episode("Offset Edge Loop");
        let start_mouse = self.viewport.viewport_mouse;
        self.start_operation(ActiveTool::PolyOffsetLoop(PolyOffsetLoopDrag {
            node_id: nid,
            original_mesh,
            edges,
            start_mouse,
            anchor_world,
            dir_pos_world,
            dir_neg_world,
            distance: 0.0,
            installed_distance: None,
            warned_refusal: false,
            warned_edge_on: false,
            both_sides: false,
            shift_was_down: true,
        }));
        // Tool Parameters dock follows the live modal (same as `poly_activate_bevel`).
        self.ui2.active_tool = "Offset Edge Loop".into();
        self.ui2.active_pillar = "poly".into();

        self.toast(if one_sided {
            "Offset Edge Loop: drag either way = offset (only one plane is legal here) · LMB/Enter confirm · Esc/RMB cancel"
        } else {
            "Offset Edge Loop: drag = offset · Shift = both planes the segment splits · LMB/Enter confirm · Esc/RMB cancel"
        });
    }

    /// Mouse → ABSOLUTE offset distance, with the side chosen by measurement (plan §7b).
    ///
    /// Re-projects both landings EVERY frame, so orbiting mid-modal keeps the mapping honest
    /// instead of freezing a stale screen axis.
    ///
    /// **Drag mapping (plan §2: "crossing zero flips the loop to the other adjacent face"):**
    /// - **Both sides legal:** one continuous signed axis. Positive projection → `+distance`
    ///   (pos side), negative → `-distance` (neg side). Crossing the arm-point flips the side.
    /// - **One side legal:** magnitude works in BOTH mouse directions (absolute projection onto
    ///   that side's screen axis). Previously `px.max(0)` discarded the opposite mouse direction,
    ///   so the tool felt one-way with edges selected whenever the probe found only one legal side
    ///   (common for open chains / face-boundary loops) or fell back to edge-on `drag.x.max(0)`.
    pub(crate) fn poly_update_offset_loop_drag(&mut self, mouse: egui::Pos2, rect: egui::Rect) {
        let Some(d) = self.tool.current.as_poly_offset_loop() else { return };
        let (start, anchor, lp, ln, prev) =
            (d.start_mouse, d.anchor_world, d.dir_pos_world, d.dir_neg_world, d.distance);

        let drag = egui::vec2(mouse.x - start.x, mouse.y - start.y);
        let axis_pos = self.offset_loop_axis(anchor, lp, rect);
        let axis_neg = self.offset_loop_axis(anchor, ln, rect);
        let scale = self.viewport.camera.distance * OFFSET_DRAG_SCALE;
        let edge_on = axis_pos.is_none() && axis_neg.is_none();
        let signed = offset_drag_signed(
            axis_pos.map(|a| drag.dot(a)),
            axis_neg.map(|a| drag.dot(a)),
            lp.is_some(),
            ln.is_some(),
            drag.x,
            scale,
        );

        // P3: proportional mode (Alt) attaches here — `signed` becomes `t ∈ [0,1)` along each rail
        // and routes to the kernel's proportional path once that path exists.

        // Warn BEFORE the no-change early-out: at arm time the drag is zero, so an edge-on view
        // would otherwise never be reported until the user had already dragged blind.
        let warn_edge_on = match self.tool.current.as_poly_offset_loop_mut() {
            Some(d) if edge_on && !d.warned_edge_on => {
                d.warned_edge_on = true;
                true
            }
            _ => false,
        };
        if warn_edge_on {
            self.toast(
                "Offset Edge Loop: the offset direction is edge-on to this view — orbit for \
                 clearer side control (drag left or right still offsets)",
            );
        }
        if (signed - prev).abs() < 1e-6 {
            return;
        }
        if let Some(d) = self.tool.current.as_poly_offset_loop_mut() {
            d.distance = signed;
        }
        self.poly_offset_loop_regenerate();
    }

    /// THE single producer of the offset result.
    ///
    /// Always rebuilds from the drag-start base — never re-offsets an already-offset mesh (the
    /// bevel modal's `original_mesh` pattern). The kernel's refusals are ATOMIC (`ops_inset.rs`
    /// § Atomicity: every check runs before the first mutation), so a refusal discards an untouched
    /// clone and the live mesh simply keeps the last good frame.
    pub(crate) fn poly_offset_loop_regenerate(&mut self) {
        let (nid, original, edges, signed) = {
            let Some(d) = self.tool.current.as_poly_offset_loop() else { return };
            (d.node_id, d.original_mesh.clone(), d.edges.clone(), d.distance)
        };

        if signed.abs() < 1e-6 {
            // Below the smallest legal offset: the body IS the drag-start base.
            self.poly_offset_loop_install(nid, original);
            if let Some(d) = self.tool.current.as_poly_offset_loop_mut() {
                d.installed_distance = None;
            }
            return;
        }

        let sel: HashSet<HalfEdgeId> = edges.iter().map(|&e| HalfEdgeId(e)).collect();
        let both = self
            .tool
            .current
            .as_poly_offset_loop()
            .map(|d| d.both_sides)
            .unwrap_or(false);
        let mut m = (*original).clone();
        let result = if both {
            m.offset_edge_loop_both(&sel, signed)
        } else {
            m.offset_edge_loop(&sel, signed)
        };
        match result {
            Ok(_) => {
                self.poly_offset_loop_install(nid, Arc::new(m));
                if let Some(d) = self.tool.current.as_poly_offset_loop_mut() {
                    d.installed_distance = Some(signed);
                }
            }
            Err(why) => {
                let warn = match self.tool.current.as_poly_offset_loop_mut() {
                    Some(d) if !d.warned_refusal => {
                        d.warned_refusal = true;
                        true
                    }
                    _ => false,
                };
                if warn {
                    self.toast(&format!("Offset Edge Loop: {why}"));
                }
            }
        }
    }

    /// Install a mesh on the poly node and re-tessellate LIVE so the surface follows the edit each
    /// frame (identical contract to the bevel install).
    ///
    /// The runtime sidecar (`cache.poly_custom_data`) is deliberately NOT re-keyed here: this op is
    /// ADD-ONLY (`split_edge` + `split_face_between` allocate and never free), and every frame
    /// rebuilds from the same `original_mesh`, so every pre-existing vertex / edge / face id the
    /// sidecar refers to keeps its meaning. That is the opposite of the bevel, which does a
    /// `from_polygons` rebuild and therefore must remap by position. (Propagating authored data
    /// onto the NEW loop's corners is plan §4 P5 and is declared `PROPAGATES = false` today.)
    fn poly_offset_loop_install(&mut self, nid: NodeId, mesh_arc: Arc<HalfEdgeMesh>) {
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                *mesh = mesh_arc;
            }
        }
        self.poly_sync_mesh_cache(nid);
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Confirm — keep the installed mesh, close the episode as ONE undo entry.
    pub(crate) fn poly_confirm_offset_loop(&mut self) {
        let Some(d) = self.tool.current.as_poly_offset_loop() else { return };
        let (nid, installed, both) = (d.node_id, d.installed_distance, d.both_sides);
        let Some(dist) = installed else {
            // Nothing was ever offset (drag never left zero, or every frame refused). Discard the
            // episode rather than committing a no-op undo entry.
            self.poly_cancel_offset_loop();
            self.toast("Offset Edge Loop cancelled (zero offset)");
            return;
        };
        self.commit_episode();
        self.tool.current = ActiveTool::Idle;
        self.poly_sync_mesh_cache(nid);
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast(&format!(
            "Offset Edge Loop applied (offset {:.3}{})",
            dist.abs(),
            if both { ", both planes" } else { "" }
        ));
    }

    /// Cancel — restore the drag-start mesh and discard the episode. SILENT: call sites toast, the
    /// same split `cancel_operation` uses for every other poly modal.
    pub(crate) fn poly_cancel_offset_loop(&mut self) {
        let Some((nid, original)) = self
            .tool
            .current
            .as_poly_offset_loop()
            .map(|d| (d.node_id, d.original_mesh.clone()))
        else {
            self.tool.current = ActiveTool::Idle;
            return;
        };
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                *mesh = original;
            }
        }
        self.cancel_episode();
        self.tool.current = ActiveTool::Idle;
        self.poly_sync_mesh_cache(nid);
        self.mark_dirty(DirtyFlags::RENDER);
    }
}

impl AzraApp {
    /// Shift tap — rail BOTH planes the selected segment splits (plan §2 / §4 P3).
    pub(crate) fn poly_toggle_offset_loop_both_sides(&mut self) {
        let (can, on) = match self.tool.current.as_poly_offset_loop() {
            Some(d) => (d.dir_pos_world.is_some() && d.dir_neg_world.is_some(), d.both_sides),
            None => return,
        };
        if !can {
            self.toast("Offset Edge Loop: only one plane is legal for this selection");
            return;
        }
        if let Some(d) = self.tool.current.as_poly_offset_loop_mut() {
            d.both_sides = !on;
        }
        self.poly_offset_loop_regenerate();
        self.toast(if !on {
            "Offset Edge Loop: both planes (each n-gon the segment splits)"
        } else {
            "Offset Edge Loop: one plane (drag through start to flip side)"
        });
    }
}

/// Pure drag→signed-distance mapping (extracted for unit tests).
///
/// Inputs are already screen-axis dots (`drag · axis`) or `None` when that side has no usable
/// screen axis. `scale` is `camera_distance * OFFSET_DRAG_SCALE`.
fn offset_drag_signed(
    score_pos: Option<f32>,
    score_neg: Option<f32>,
    pos_legal: bool,
    neg_legal: bool,
    drag_x: f32,
    scale: f32,
) -> f32 {
    match (score_pos, score_neg) {
        (Some(p), Some(n)) => {
            if p.abs() >= n.abs() {
                p * scale
            } else {
                -n * scale
            }
        }
        (Some(p), None) => p.abs() * scale,
        (None, Some(n)) => -n.abs() * scale,
        (None, None) => {
            let s = if pos_legal {
                1.0
            } else if neg_legal {
                -1.0
            } else {
                1.0
            };
            s * drag_x.abs() * scale
        }
    }
}

#[cfg(test)]
mod offset_loop_sign_tests {
    //! The P4 gate that matters: **the drag→sign binding must be measured, not assumed.**
    //!
    //! Plan §7b — `distance`'s sign selects the side by reversing the ring, and the ring's
    //! orientation falls out of `edge_loop_rings`' lowest-vertex-id start. These tests exercise
    //! `probe_landing`, the exact function the tool binds its sign with, and prove the binding is
    //! derived from where the loop LANDS rather than from a fixed sign→side correspondence.
    use super::{offset_drag_signed, probe_landing};
    use core_math::half_edge::{FaceId, HalfEdgeMesh};
    use glam::{Mat4, Vec3};
    use std::collections::HashSet;

    /// The P1 gate's own fixture: a box of half-extent 1 (rungs 2.0 long), selection = the boundary
    /// loop of face `f`, exactly as `poly_sel.selected_edges` would hold it.
    fn cube_face_loop(f: u32) -> (HalfEdgeMesh, HashSet<core_math::half_edge::HalfEdgeId>) {
        let m = HalfEdgeMesh::new_box(Vec3::ONE);
        let sel = m.face_half_edges(FaceId(f)).into_iter().collect();
        (m, sel)
    }

    #[test]
    fn probe_reports_where_the_loop_lands_not_which_sign_was_asked() {
        let (m, sel) = cube_face_loop(0);
        let probe = 0.05f32;
        let a = probe_landing(&m, &sel, probe, Mat4::IDENTITY);
        let b = probe_landing(&m, &sel, -probe, Mat4::IDENTITY);
        // Exactly one side of a cube face's boundary loop is offsettable (the other would have to
        // split the same face twice — that is the Inset tool's job, and P1 refuses it). Whichever
        // one it is, the tool learns it from the RESULT, which is the whole point.
        assert!(
            a.is_ok() != b.is_ok(),
            "a cube face boundary loop must have exactly one legal side; got +={:?} -={:?}",
            a.as_ref().err(),
            b.as_ref().err()
        );
    }

    #[test]
    fn probe_landing_moves_off_the_source_loop() {
        let (m, sel) = cube_face_loop(0);
        // Source loop centroid = the face centre.
        let face_c = {
            let vs = m.face_vertices(FaceId(0));
            vs.iter().fold(Vec3::ZERO, |a, &v| a + m.vert(v).position) / vs.len() as f32
        };
        let probe = 0.05f32;
        let land = probe_landing(&m, &sel, probe, Mat4::IDENTITY)
            .or_else(|_| probe_landing(&m, &sel, -probe, Mat4::IDENTITY))
            .expect("one side must be legal on a cube");
        // The landing must be a MEASURABLY different point, or the screen axis the tool derives
        // from it would be noise.
        assert!(
            (land - face_c).length() > 1e-4,
            "landing {land:?} is indistinguishable from the source loop centroid {face_c:?}"
        );
    }

    /// A torus minor ring (`new_torus` indexes verts as `major*NS + minor`, so `0..NS` is the ring
    /// at major segment 0). Every vertex is valence 4, so BOTH sides are offsettable — the two-way
    /// case a cube face loop cannot exercise. Returns `(mesh, selection, loop centroid)`.
    ///
    /// The selection is built by scanning for the ring's directed half-edges through the crate's
    /// PUBLIC api (`he_source` / `he`) — `find_half_edge` is core_math-private.
    fn torus_minor_ring() -> (HalfEdgeMesh, HashSet<core_math::half_edge::HalfEdgeId>, Vec3) {
        use core_math::half_edge::{HalfEdgeId, VertexId};
        const NS: u32 = 16;
        let m = HalfEdgeMesh::new_torus(1.0, 0.3, 24, NS);
        let want: HashSet<(u32, u32)> = (0..NS).map(|j| (j, (j + 1) % NS)).collect();
        let sel: HashSet<HalfEdgeId> = (0..m.half_edges.len() as u32)
            .map(HalfEdgeId)
            .filter(|&h| m.is_edge_live(h))
            .filter(|&h| want.contains(&(m.he_source(h).0, m.he(h).vertex.0)))
            .collect();
        assert_eq!(sel.len(), NS as usize, "one directed half-edge per ring edge");
        let anchor = (0..NS).map(|j| m.vert(VertexId(j)).position).fold(Vec3::ZERO, |a, b| a + b)
            / NS as f32;
        (m, sel, anchor)
    }

    /// Both signs are legal on a torus minor ring, so the tool has a genuine two-way drag mapping.
    /// The two probe DIRECTIONS must be opposite — that is what makes "drag the other way flips the
    /// loop to the other adjacent face" (plan §2) fall out of the MEASUREMENT instead of being
    /// asserted by the tool.
    #[test]
    fn both_legal_sides_probe_to_opposite_directions() {
        let (base, sel, anchor) = torus_minor_ring();
        let probe = 0.02f32;
        let dp = probe_landing(&base, &sel, probe, Mat4::IDENTITY).expect("+ side is legal");
        let dn = probe_landing(&base, &sel, -probe, Mat4::IDENTITY).expect("- side is legal");
        let up = (dp - anchor).normalize_or_zero();
        let un = (dn - anchor).normalize_or_zero();
        assert!(up.length_squared() > 0.5 && un.length_squared() > 0.5, "both directions must exist");
        assert!(
            up.dot(un) < -0.9,
            "the two signs must land on OPPOSITE sides; got dot {} (+{up:?} / -{un:?})",
            up.dot(un)
        );
    }

    /// The probe magnitude must not change the measured DIRECTION. This is what licenses the tiny
    /// `PROBE_FRACTION`: the kernel places each new vertex at `V + d · normalize(rail − V)`, a pure
    /// scaling, so below the per-vertex rail clamp the direction is exact at any `d`.
    #[test]
    fn probe_direction_is_independent_of_probe_magnitude() {
        let (base, sel, anchor) = torus_minor_ring();
        let dir_at = |d: f32| {
            (probe_landing(&base, &sel, d, Mat4::IDENTITY).expect("legal side") - anchor)
                .normalize_or_zero()
        };
        let a = dir_at(0.005);
        let b = dir_at(0.05);
        assert!(a.dot(b) > 0.999, "direction drifted with probe size: {a:?} vs {b:?}");
    }

    #[test]
    fn probe_refusal_leaves_the_input_mesh_untouched() {
        // Fail-closed contract: the tool probes BEFORE opening an undo episode, which is only
        // sound because a refusal mutates nothing the caller can see.
        let (m, sel) = cube_face_loop(0);
        let before = m.content_signature();
        let _ = probe_landing(&m, &sel, 0.05, Mat4::IDENTITY);
        let _ = probe_landing(&m, &sel, -0.05, Mat4::IDENTITY);
        assert_eq!(before, m.content_signature(), "probe must not touch the caller's mesh");
    }

    /// Regression: Offset Edge Loop with edges selected felt "one direction only".
    /// Cause: `px.max(0)` / `drag.x.max(0)` discarded the opposite mouse direction.
    #[test]
    fn drag_mapping_both_mouse_directions_produce_offset() {
        let scale = 1.0;
        // Two-sided: left and right along the primary axis flip the sign (cross zero).
        let right = offset_drag_signed(Some(100.0), Some(-100.0), true, true, 0.0, scale);
        let left = offset_drag_signed(Some(-100.0), Some(100.0), true, true, 0.0, scale);
        assert!(right > 0.0, "drag toward + side must be +distance, got {right}");
        assert!(left < 0.0, "drag toward - side must be -distance, got {left}");
        assert!((right + left).abs() < 1e-5, "opposite drags must cancel in magnitude");

        // One-sided (+ only): BOTH mouse directions still produce a NON-ZERO + offset.
        let toward = offset_drag_signed(Some(50.0), None, true, false, 0.0, scale);
        let away = offset_drag_signed(Some(-50.0), None, true, false, 0.0, scale);
        assert!(toward > 0.0 && away > 0.0, "one-sided must accept either mouse way: {toward}/{away}");
        assert!((toward - away).abs() < 1e-5);

        // One-sided (− only): either mouse way → negative signed distance.
        let a = offset_drag_signed(None, Some(40.0), false, true, 0.0, scale);
        let b = offset_drag_signed(None, Some(-40.0), false, true, 0.0, scale);
        assert!(a < 0.0 && b < 0.0, "neg-only one-sided: {a}/{b}");

        // Edge-on fallback: left AND right horizontal drag must offset (old bug: only drag.x > 0).
        let er = offset_drag_signed(None, None, true, false, 80.0, scale);
        let el = offset_drag_signed(None, None, true, false, -80.0, scale);
        assert!(er > 0.0 && el > 0.0, "edge-on must accept left and right: {er}/{el}");
        assert!((er - el).abs() < 1e-5);
    }
}
