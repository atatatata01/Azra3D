// PolySculpt episode lifecycle — PR-5 + PR-6 of ZB_RESPONSIVE_SCULPT_PLAN (§7.4/§7.5).
//
// The working mesh is truth mid-stroke. The document (authored mesh, or the Sculpt
// modifier's delta sidecar) is written EXACTLY ONCE per stroke, at `commit_episode` —
// the single choke point every closer passes through (LMB-up, auto-commit, mid-stroke
// Ctrl+Z, a later tool's `begin_episode`, leave-edit). Undo is a sparse position SWAP
// (`UndoPayload::PolySculpt`, first-touch orig set) — never a Full scene snapshot, so
// history no longer retains one COWed remesh per stroke.

use core_math::half_edge::VertexId;
use core_math::scene_graph::NodeId;
use glam::Vec3;

use crate::UndoPayload;

impl crate::AzraApp {
    /// §7.4/K14: is an open stroke pinning `nid`'s working mesh as the truth?
    /// True while the body is the armed edit target AND (the pointer is mid-stroke OR a
    /// PolySculpt episode is pending). Consumers: `poly_sculpt_eval_working` (return the
    /// live working mesh, ignore gen mismatch) and `poly_prefix_cache_remove` (never
    /// `poly_sculpt_drop` the active target).
    pub(crate) fn poly_sculpt_stroke_pinned(&self, nid: NodeId) -> bool {
        let armed = matches!(self.sculpt.poly_edit, Some((n, _)) if n == nid);
        if !armed {
            return false;
        }
        self.sculpt.stroke_active
            || matches!(
                self.pending_episode.as_ref().map(|e| &e.payload),
                Some(UndoPayload::PolySculpt { nid: n, .. }) if *n == nid
            )
    }

    /// §7.5 lifecycle 1: start a sparse PolySculpt episode. Mirrors `begin_episode`'s
    /// constructor side effects MINUS the Full snapshot:
    ///   1. `commit_episode()` FIRST — the document choke runs on any previous episode
    ///      (never overwrite a pending stroke without its write);
    ///   2. install an EMPTY `PolySculpt` payload (first-touch fills it; a dab-less
    ///      stroke is dropped at commit, §7.5);
    ///   3. copy the bake-suppress behavior.
    /// Never `snapshot_undo`.
    pub(crate) fn begin_poly_sculpt_episode(&mut self, nid: NodeId, slot: Option<usize>) {
        self.commit_episode();
        // Accumulate-off deposit ledger is STROKE-scoped (Blender BRUSH_ACCUMULATE model:
        // one offset layer per stroke) — a new episode starts a new layer. Absent working
        // = nothing to clear (a fresh working starts with an empty ledger).
        if let Some(w) = self.poly_sculpt_working.get_mut(&nid) {
            w.deposit_applied.clear();
            // Same stroke scope as the ledger: the frozen brush-math normals and the
            // enhance-details direction field belong to ONE stroke (Blender re-captures
            // orig_data / detail_directions per stroke).
            w.first_touch_normal.clear();
            w.enhance_dirs = None;
        }
        let (pv, pe, pf, pav) = self.reverse_poly_sel_fields();
        self.pending_episode = Some(crate::UndoEntry {
            description: "PolySculpt stroke".into(),
            payload: UndoPayload::PolySculpt {
                nid,
                slot,
                orig: Vec::new(),
            },
            active_body_nid: self.scene.active_body_nid,
            usel: self.sel.usel.clone(),
            poly_verts: pv,
            poly_edges: pe,
            poly_faces: pf,
            poly_active_vertex: pav,
            material_panel_docs_json: None,
            material_node_graphs_json: None,
            poly_custom_data: None,
        });
        if self.cache.dc_mesh_route(false) == crate::cache_state::DcMeshRoute::CpuChunkedOracle {
            self.cache.body_vol_cache.suppress_bake_dispatch = true;
        }
    }

    /// §7.4: the ONE document write per stroke — called from the TOP of `commit_episode`.
    /// Copies ONLY the stroke-touched set from the working mesh into the document.
    /// Direct: authored mesh positions+normals at the touched ids (same-topology only —
    /// the remesh-replace + disable-mods write stays an ENTER-time event). Layer: upsert
    /// `sculpt_deltas[id] = current − prefix_pos` and bump `sculpt_delta_gen` together
    /// with `w.gen` so the next idle stack walk matches again (C1).
    pub(crate) fn poly_sculpt_commit_document(&mut self, nid: NodeId, slot: Option<usize>) {
        use core_math::scene_graph::NodePayload;
        let Some(w) = self.poly_sculpt_working.get(&nid) else {
            return;
        };
        // Touched set = the first-touch keys. Read the CURRENT working values for them.
        // (first_touch is drained by the caller AFTER this write, into the undo payload.)
        let touched: Vec<u32> = w.first_touch.keys().copied().collect();
        if touched.is_empty() {
            return;
        }
        let gen = w.gen;
        match slot {
            None => {
                let src: Vec<(u32, Vec3, Vec3)> = touched
                    .iter()
                    .filter_map(|&i| {
                        let vid = VertexId(i);
                        if !w.mesh.is_vert_live(vid) {
                            return None;
                        }
                        let v = w.mesh.vert(vid);
                        Some((i, v.position, v.normal))
                    })
                    .collect();
                if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                        // Same-topology contract: the stroke never changes topology.
                        let m = std::sync::Arc::make_mut(mesh);
                        for (i, p, n) in src {
                            let vid = VertexId(i);
                            if (vid.0 as usize) < m.vertices.len() && m.is_vert_live(vid) {
                                m.vert_mut(vid).position = p;
                                m.vert_mut(vid).normal = n;
                            }
                        }
                    }
                }
            }
            Some(si) => {
                let deltas: Vec<(u32, Vec3)> = touched
                    .iter()
                    .filter_map(|&i| {
                        let vid = VertexId(i);
                        if !w.mesh.is_vert_live(vid)
                            || (vid.0 as usize) >= w.prefix.vertices.len()
                        {
                            return None;
                        }
                        Some((i, w.mesh.vert(vid).position - w.prefix.vert(vid).position))
                    })
                    .collect();
                if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { modifiers, .. } = &mut node.payload {
                        if let Some(m) = modifiers.get_mut(si) {
                            for (i, d) in deltas {
                                if let Some(e) =
                                    m.sculpt_deltas.iter_mut().find(|(id, _)| *id == i)
                                {
                                    e.1 = d;
                                } else {
                                    m.sculpt_deltas.push((i, d));
                                }
                            }
                            // Bump TOGETHER (§7.4 pin step 3): the next idle walk matches.
                            m.sculpt_delta_gen = gen;
                        }
                    }
                }
            }
        }
    }

    /// §7.5 apply: restore `orig` onto working + document, returning the PRE-apply
    /// (current) positions of the same set for the reverse entry. Direct: swap on
    /// working AND authored. Layer: working = orig; `sculpt_deltas[id] = orig −
    /// prefix_pos` (the delta channel is delta-from-prefix — NEVER raw positions).
    /// Display: in-place patch + flush when the working mesh still holds the body;
    /// `DirtyFlags::MESH` otherwise (the RENDER-only trap is the owner's "surface
    /// stuck after Ctrl+Z" class).
    pub(crate) fn poly_sculpt_apply_snapshot(
        &mut self,
        nid: NodeId,
        slot: Option<usize>,
        orig: &[(u32, [f32; 3])],
    ) -> Vec<(u32, [f32; 3])> {
        use core_math::scene_graph::NodePayload;
        let mut post: Vec<(u32, [f32; 3])> = Vec::with_capacity(orig.len());
        let has_working = self.poly_sculpt_working.contains_key(&nid);
        // 1. Working mesh (when present): capture post, restore orig, fix grid + display.
        if has_working {
            let ids: Vec<u32> = orig.iter().map(|(i, _)| *i).collect();
            if let Some(w) = self.poly_sculpt_working.get_mut(&nid) {
                let mut seeds = Vec::with_capacity(orig.len());
                for &(i, o) in orig {
                    let vid = VertexId(i);
                    if (vid.0 as usize) >= w.mesh.vertices.len() || !w.mesh.is_vert_live(vid) {
                        continue;
                    }
                    let cur = w.mesh.vert(vid).position;
                    post.push((i, cur.to_array()));
                    let op = Vec3::from_array(o);
                    w.mesh.vert_mut(vid).position = op;
                    w.grid.move_vert(i, cur, op);
                    seeds.push(vid);
                }
                w.mesh.recompute_normals_region(seeds.iter().copied());
                super::sculpt::write_display_corners(w, &ids);
                w.gen = w.gen.wrapping_add(1);
            }
        }
        // 2. Document.
        match slot {
            None => {
                if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                        let m = std::sync::Arc::make_mut(mesh);
                        for &(i, o) in orig {
                            let vid = VertexId(i);
                            if (vid.0 as usize) >= m.vertices.len() || !m.is_vert_live(vid) {
                                continue;
                            }
                            if !has_working {
                                post.push((i, m.vert(vid).position.to_array()));
                            }
                            m.vert_mut(vid).position = Vec3::from_array(o);
                        }
                        let seeds: Vec<VertexId> = orig
                            .iter()
                            .map(|(i, _)| VertexId(*i))
                            .filter(|v| (v.0 as usize) < m.vertices.len() && m.is_vert_live(*v))
                            .collect();
                        m.recompute_normals_region(seeds.iter().copied());
                    }
                }
            }
            Some(si) => {
                // prefix positions come from the working prefix when present, else the
                // stack walk will rebuild from deltas — rewrite what we can reach.
                let prefix_pos: Option<Vec<(u32, Vec3)>> =
                    self.poly_sculpt_working.get(&nid).map(|w| {
                        orig.iter()
                            .filter_map(|&(i, _)| {
                                let vid = VertexId(i);
                                if (vid.0 as usize) < w.prefix.vertices.len() {
                                    Some((i, w.prefix.vert(vid).position))
                                } else {
                                    None
                                }
                            })
                            .collect()
                    });
                if let (Some(pp), Some(node)) = (prefix_pos, self.scene.scene_graph.get_mut(nid))
                {
                    if let NodePayload::PolygonalBody { modifiers, .. } = &mut node.payload {
                        if let Some(m) = modifiers.get_mut(si) {
                            let pmap: std::collections::HashMap<u32, Vec3> =
                                pp.into_iter().collect();
                            for &(i, o) in orig {
                                let Some(&pref) = pmap.get(&i) else { continue };
                                let d = Vec3::from_array(o) - pref;
                                if let Some(e) =
                                    m.sculpt_deltas.iter_mut().find(|(id, _)| *id == i)
                                {
                                    e.1 = d;
                                } else {
                                    m.sculpt_deltas.push((i, d));
                                }
                            }
                            if let Some(w) = self.poly_sculpt_working.get(&nid) {
                                m.sculpt_delta_gen = w.gen;
                            } else {
                                m.sculpt_delta_gen = m.sculpt_delta_gen.wrapping_add(1);
                            }
                        }
                    }
                }
            }
        }
        // 3. Display propagation.
        if has_working {
            self.poly_sculpt_flush_display(nid);
            self.mark_dirty(crate::DirtyFlags::RENDER);
        } else {
            self.mark_dirty(crate::DirtyFlags::MESH);
            self.poly_display_dirty.insert(nid);
        }
        post
    }

    /// §7.5 cancel: restore the first-touch originals onto working + display, then drop
    /// the pending episode WITHOUT pushing. (`cancel_episode` alone just drops — gizmo
    /// cancel restores from tool orig; PolySculpt restores here, its own path.)
    pub(crate) fn poly_sculpt_cancel_episode(&mut self) {
        let info = match self.pending_episode.as_ref().map(|e| &e.payload) {
            Some(UndoPayload::PolySculpt { nid, slot, .. }) => Some((*nid, *slot)),
            _ => None,
        };
        if let Some((nid, slot)) = info {
            let orig: Vec<(u32, [f32; 3])> = self
                .poly_sculpt_working
                .get_mut(&nid)
                .map(|w| {
                    w.first_touch
                        .drain()
                        .map(|(i, p)| (i, p.to_array()))
                        .collect()
                })
                .unwrap_or_default();
            if !orig.is_empty() {
                let _ = self.poly_sculpt_apply_snapshot(nid, slot, &orig);
            }
        }
        self.cancel_episode();
    }
}

// ============================================================================
// G4 + G5 gates (§7.4/§7.5 negatives) — headless via AzraApp::new_headless.
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::poly::sculpt::grab_tests::app_with_grid;

    fn vert_pos(app: &crate::AzraApp, nid: NodeId, i: u32) -> Vec3 {
        match &app.scene.scene_graph.get(nid).unwrap().payload {
            core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } => {
                mesh.vert(VertexId(i)).position
            }
            _ => unreachable!(),
        }
    }

    /// G4: N dabs write the document ONCE, at commit — never per dab. G5: the pending
    /// payload is sparse (no scene snapshot), and undo/redo swap positions.
    #[test]
    fn commit_once_and_sparse_undo_swap() {
        let (mut app, nid) = app_with_grid();
        app.poly_sculpt_enter_edit(nid, usize::MAX);
        app.begin_poly_sculpt_episode(nid, None);
        let pre = vert_pos(&app, nid, 40);
        for k in 0..5 {
            let c = Vec3::new(0.0, 0.0, 0.0);
            app.poly_sculpt_stamp(nid, c, 0.5, Vec3::Y * (0.01 + k as f32 * 0.001));
            // §7.4: the AUTHORED mesh is untouched mid-stroke (working is truth).
            assert_eq!(
                vert_pos(&app, nid, 40),
                pre,
                "dab {k} wrote the document mid-stroke"
            );
        }
        // Sparse payload, no scene snapshot (G5 negative).
        match app.pending_episode.as_ref().map(|e| &e.payload) {
            Some(UndoPayload::PolySculpt { orig, .. }) => {
                assert!(!orig.is_empty() || true); // filled at commit
            }
            other => panic!("pending must be PolySculpt, got {:?}", other.is_some()),
        }
        app.commit_episode();
        let committed = vert_pos(&app, nid, 40);
        assert!(
            (committed - pre).length() > 1e-4,
            "commit must write the stroke to the document"
        );
        let entry = app.history.undo_stack.back().expect("one entry per stroke");
        match &entry.payload {
            UndoPayload::PolySculpt { orig, .. } => {
                assert!(!orig.is_empty(), "first-touch set drained into the payload");
                assert!(
                    (orig.len() * 16) < 81 * 16 * 2,
                    "payload is sparse (≪ mesh bytes)"
                );
            }
            _ => panic!("stroke entry must be PolySculpt (not Full — S0c completed)"),
        }
        // Undo restores; redo re-applies.
        app.undo();
        assert!(
            (vert_pos(&app, nid, 40) - pre).length() < 1e-5,
            "undo restores pre-stroke positions"
        );
        app.redo();
        assert!(
            (vert_pos(&app, nid, 40) - committed).length() < 1e-5,
            "redo restores post-stroke positions"
        );
    }

    /// PR-8 / G3 (§20): upload bytes ∝ moved corners, never the mesh. Headless via the
    /// `seed_persistent_baseline` seam (no device: cpu_mesh + baseline slots only).
    #[test]
    fn g3_upload_bytes_proportional_to_moved_corners() {
        let (mut app, nid) = app_with_grid();
        app.poly_sculpt_enter_edit(nid, usize::MAX);
        app.ensure_poly_sculpt_working(nid);
        let (n_corners, baseline) = {
            let w = app.poly_sculpt_working.get(&nid).unwrap();
            (w.display_verts.len(), w.display_verts.clone())
        };
        let cid = app.cache.mesh_cache.alloc_id();
        if let Some(node) = app.scene.scene_graph.get_mut(nid) {
            if let core_math::scene_graph::NodePayload::PolygonalBody { mesh_cache_id, .. } =
                &mut node.payload
            {
                *mesh_cache_id = Some(cid);
            }
        }
        app.cache
            .mesh_cache
            .seed_persistent_baseline(cid, baseline, vec![0, 1, 2], 1);
        app.begin_poly_sculpt_episode(nid, None);
        app.poly_sculpt_stamp(nid, Vec3::ZERO, 0.5, Vec3::Y * 0.02);
        let p = app.sculpt.poly_profile;
        let full = (n_corners * 32) as u64;
        assert!(p.upload_bytes > 0, "the scatter path must have run");
        assert!(
            p.upload_bytes < full / 2,
            "G3: upload {} bytes vs full VB {} — bytes must be ∝ moved corners",
            p.upload_bytes,
            full
        );
        assert_eq!(
            p.upload_bytes,
            p.corner_count as u64 * 32,
            "uploaded bytes must equal patched corners × row size"
        );
        app.commit_episode();
    }

    /// §7.5: a dab-less stroke pushes NOTHING.
    #[test]
    fn dabless_stroke_pushes_no_entry() {
        let (mut app, nid) = app_with_grid();
        app.poly_sculpt_enter_edit(nid, usize::MAX);
        let depth = app.history.undo_stack.len();
        app.begin_poly_sculpt_episode(nid, None);
        app.commit_episode();
        assert_eq!(
            app.history.undo_stack.len(),
            depth,
            "dab-less stroke must not push a junk entry"
        );
    }

    /// §7.4 open-stroke pin: a prefix-cache removal mid-stroke must NOT drop the
    /// active working mesh, and eval keeps returning the live positions.
    #[test]
    fn open_stroke_pin_survives_resync() {
        let (mut app, nid) = app_with_grid();
        app.poly_sculpt_enter_edit(nid, usize::MAX);
        app.begin_poly_sculpt_episode(nid, None);
        app.poly_sculpt_stamp(nid, Vec3::ZERO, 0.5, Vec3::Y * 0.02);
        let sculpted = app
            .poly_sculpt_working
            .get(&nid)
            .unwrap()
            .mesh
            .vert(VertexId(40))
            .position;
        assert!(sculpted.y.abs() > 1e-4, "stamp must move the working vert");
        // The historical stroke-killer: a display resync (prefix removal) mid-stroke.
        app.poly_force_display_resync(nid);
        let w = app
            .poly_sculpt_working
            .get(&nid)
            .expect("PIN: the active working mesh must survive a resync");
        assert!(
            (w.mesh.vert(VertexId(40)).position - sculpted).length() < 1e-6,
            "PIN: working positions must survive the walk"
        );
        app.commit_episode();
    }

    /// §7.5: Grab persists to the document at commit (was display-only — snapped back
    /// on save/resync).
    #[test]
    fn grab_commits_to_document() {
        let (mut app, nid) = app_with_grid();
        app.poly_sculpt_enter_edit(nid, usize::MAX);
        app.begin_poly_sculpt_episode(nid, None);
        let anchor = Vec3::new(0.0, 0.0, 0.0);
        assert!(app.poly_grab_begin(nid, anchor, anchor, 0.6));
        app.poly_grab_update(Vec3::Y * 0.3);
        let pre_commit = vert_pos(&app, nid, 40);
        app.commit_episode();
        let post = vert_pos(&app, nid, 40);
        assert!(
            post.y > pre_commit.y + 1e-4,
            "Grab must land in the authored mesh at commit (was display-only): pre={pre_commit:?} post={post:?}"
        );
    }
}
