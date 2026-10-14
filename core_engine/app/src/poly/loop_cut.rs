use std::sync::Arc;
use eframe::egui;
use glam::Vec3;
use core_math::half_edge::HalfEdgeId;
use core_math::scene_graph::{NodeId, NodePayload};
use crate::{AzraApp, DirtyFlags, PolyTool, ActiveTool, PolyLoopCutRing, PolyLoopCutSlide, world_to_screen};

/// Ceiling on rings cut in one session. A full edge loop on a dense cylinder is legitimately
/// hundreds of rings, so this is deliberately generous; it exists because LIVE mode re-cuts every
/// ring on every drag frame (Frame Budget Rule) and a select-all would otherwise make the slide
/// unusable. Clamping is TOASTED, never silent.
const MAX_LOOP_CUT_RINGS: usize = 256;

/// Bare-cursor travel that engages the slide, in screen pixels.
///
/// The session has two phases and NO click to separate them (the click accepts), so the separator is
/// distance: while the ring still follows the hovered edge every retarget re-anchors, and travel this
/// far without a retarget means the user has settled on a ring and is now moving it. Small enough to
/// feel immediate, large enough that pointer jitter cannot lock a ring the user was still choosing.
const SLIDE_ENGAGE_PX: f32 = 8.0;

impl AzraApp {
    /// Cut fractions for `count` loops slid by `factor`, optional `paired` support-loop mode.
    /// Normal: `count` evenly-spaced loops. Paired: `count` mirrored PAIRS straddling the centre (the
    /// classic support-edge pair), spread by |factor|. All kept inside (0.02,0.98) so a slid loop
    /// never collapses onto a rail endpoint.
    pub(crate) fn loop_cut_fractions(count: u32, factor: f32, paired: bool) -> Vec<f32> {
        let count = count.max(1);
        if paired {
            let spread = 0.45 * factor.abs().max(0.15); // S is instantly useful even at factor 0
            let mut out = Vec::with_capacity(count as usize * 2);
            for i in 1..=count {
                let d = spread * (i as f32 / count as f32);
                out.push((0.5 - d).clamp(0.02, 0.98));
                out.push((0.5 + d).clamp(0.02, 0.98));
            }
            out.sort_by(|a, b| a.partial_cmp(b).unwrap());
            out
        } else {
            let spacing = 1.0 / (count as f32 + 1.0);
            (1..=count).map(|i| (i as f32 * spacing + factor * spacing).clamp(0.02, 0.98)).collect()
        }
    }

    /// Resolve ONE ring group (`seed` + the selected edges on its ring) into a cut spec.
    ///
    /// Extracted so the single-ring hover session and every ring of a multi-edge selection share
    /// one rule set and cannot drift:
    /// * a group of 2+ selected edges limits the span (partial cut),
    /// * a single-edge group is a free full ring — exactly like hovering it,
    /// * a limit that kills the ring while the unlimited ring works (stale / wrong-orientation
    ///   selection) is DROPPED rather than silently degrading to segments,
    /// * a seed with no quad ring at all falls back to segment mode over the group's edges.
    pub(crate) fn loop_cut_resolve_ring(
        mesh: &core_math::half_edge::HalfEdgeMesh,
        seed: u32,
        members: std::collections::HashSet<u32>,
    ) -> PolyLoopCutRing {
        let mut limit = if members.len() >= 2 { Some(members.clone()) } else { None };
        let mut has_loop = mesh.loop_cut_ring_has_quads(HalfEdgeId(seed), limit.as_ref());
        if !has_loop && limit.is_some() && mesh.loop_cut_ring_has_quads(HalfEdgeId(seed), None) {
            limit = None;
            has_loop = true;
        }
        if has_loop {
            PolyLoopCutRing { start_edge: seed, limit, segment_mode: false }
        } else {
            PolyLoopCutRing { start_edge: seed, limit: Some(members), segment_mode: true }
        }
    }

    /// Resolve a whole edge selection into the rings a cut will run on — the one place that turns
    /// "what is selected" into "what gets cut".
    ///
    /// Ring groups map 1:1 to loop cuts, but SEGMENT-mode groups do NOT: `edge_series_cut` works by
    /// connecting cut points that share a face, so handing it one edge at a time would split each
    /// edge and never join them — a chain of non-ring edges would stop producing cut segments. All
    /// segment-mode groups therefore collapse back into ONE segment ring over their union, which is
    /// what the whole selection used to be fed as.
    pub(crate) fn loop_cut_rings_from_selection(
        mesh: &core_math::half_edge::HalfEdgeMesh,
        selected: &std::collections::HashSet<u32>,
    ) -> Vec<PolyLoopCutRing> {
        let groups = mesh.loop_cut_ring_groups(selected);
        let mut rings: Vec<PolyLoopCutRing> = Vec::with_capacity(groups.len());
        let mut segment_seed: Option<u32> = None;
        let mut segment_edges: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for (seed, members) in groups {
            let r = Self::loop_cut_resolve_ring(mesh, seed, members);
            if r.segment_mode {
                segment_seed.get_or_insert(r.start_edge);
                segment_edges.extend(Self::loop_cut_segment_edges(&r));
            } else {
                rings.push(r);
            }
        }
        if let Some(seed) = segment_seed {
            rings.push(PolyLoopCutRing {
                start_edge: seed,
                limit: Some(segment_edges),
                segment_mode: true,
            });
        }
        rings
    }

    /// Every ring the session cuts: the primary (slide rail / preview / R / K seed) first, then any
    /// extras from a multi-edge selection. Exactly one entry for a normal hover session.
    pub(crate) fn loop_cut_session_rings(d: &PolyLoopCutSlide) -> Vec<PolyLoopCutRing> {
        let mut out = Vec::with_capacity(1 + d.extra_rings.len());
        out.push(PolyLoopCutRing {
            start_edge: d.start_edge,
            limit: d.limit.clone(),
            segment_mode: d.segment_mode,
        });
        out.extend(d.extra_rings.iter().cloned());
        out
    }

    /// The edges a ring cuts in segment mode: its own span, or the seed alone.
    pub(crate) fn loop_cut_segment_edges(r: &PolyLoopCutRing) -> std::collections::HashSet<u32> {
        r.limit.clone().unwrap_or_else(|| {
            let mut s = std::collections::HashSet::new();
            s.insert(r.start_edge);
            s
        })
    }

    /// Follow-curvature strength a NEW cut should use: the tool-settings amount when the toggle is
    /// on, else 0 (straight chord). One place, so the session, the one-shot and the preview agree.
    pub(crate) fn loop_cut_effective_curvature(&self) -> f32 {
        if self.loop_cut_follow_curvature {
            self.loop_cut_curvature
        } else {
            0.0
        }
    }

    /// Apply every ring's cut to `m` in sequence; returns the vertices each ring inserted (index
    /// matches `rings`), so callers can attribute result geometry back to the ring that made it.
    ///
    /// Rings are independent by construction (a rail belongs to one ring), but an earlier cut does
    /// add rails a later ring's walk will meet, and those rails are not in that ring's span limit —
    /// so a limit that ends up blocking its OWN ring is dropped for that ring. The rescue is gated
    /// on `i > 0`: the first ring is verified against the pre-cut mesh at session start, so an empty
    /// result there is a genuine "nothing to insert" and must stay one.
    ///
    /// `curvature` > 0 lifts each inserted vertex off the straight rail chord onto the arc through
    /// its rail's endpoint normals — per ring, from that ring's own delta, so the parent rail and
    /// the position along it are exact even for multi-cut. SEGMENT mode is exempt: `edge_series_cut`
    /// reports no parent map, and a lone edge with no ring has no strip curvature to follow.
    fn loop_cut_apply_rings(
        m: &mut core_math::half_edge::HalfEdgeMesh,
        rings: &[PolyLoopCutRing],
        ts: &[f32],
        curvature: f32,
    ) -> Vec<Vec<u32>> {
        let mut out = Vec::with_capacity(rings.len());
        for (i, r) in rings.iter().enumerate() {
            let verts: Vec<u32> = if r.segment_mode {
                let edges = Self::loop_cut_segment_edges(r);
                m.edge_series_cut(&edges, ts).into_iter().map(|v| v.0).collect()
            } else {
                let he = HalfEdgeId(r.start_edge);
                let (mut v, mut delta) = m.loop_cut_multi_with_delta(he, ts, r.limit.as_ref());
                if v.is_empty() && i > 0 && r.limit.is_some() {
                    let (v2, d2) = m.loop_cut_multi_with_delta(he, ts, None);
                    v = v2;
                    delta = d2;
                }
                m.apply_loop_cut_curvature(&delta, curvature);
                v.into_iter().map(|x| x.0).collect()
            };
            out.push(verts);
        }
        out
    }

    /// Commit a settled loop cut of `count` centered loops (no slide) — the keyboard one-shot. ONE undo.
    ///
    /// Dry-runs on a clone first: `transaction()` always pushes, so a degenerate edge that inserts
    /// nothing must not open history (same honesty as interactive confirm / fill / bridge).
    pub(crate) fn poly_loop_cut_commit(&mut self, nid: NodeId, start_edge: u32, count: u32) -> bool {
        let ts = Self::loop_cut_fractions(count.max(1), 0.0, false);
        let curvature = self.loop_cut_effective_curvature();
        let will_insert = {
            let Some(node) = self.scene.scene_graph.get(nid) else { return false };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else { return false };
            let mut scratch = (**mesh).clone();
            !scratch.loop_cut_multi(HalfEdgeId(start_edge), &ts, None).is_empty()
        };
        if !will_insert {
            return false;
        }
        self.transaction(&format!("Loop Cut x{}", count.max(1)), |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let m = Arc::make_mut(mesh);
                    let (_, delta) = m.loop_cut_multi_with_delta(HalfEdgeId(start_edge), &ts, None);
                    m.apply_loop_cut_curvature(&delta, curvature);
                }
            }
            s.poly_request_display_sync(nid);
            s.poly_sel.selected_vertices.clear();
            s.poly_sel.selected_edges.clear();
            s.poly_sel.selected_faces.clear();
            s.poly_invalidate_pick_cache();
            s.mark_dirty(DirtyFlags::RENDER);
        });
        // NO auto-group here — see the note at the other loop-cut confirm site. A group containing
        // EVERY face makes Group-mode clicking select the whole mesh.
        true
    }

    /// One-shot multi-ring cut driven by the edge selection: one centered loop per independent
    /// ring, ONE undo. Returns false with the mesh untouched (and no history entry) when the
    /// selection resolves to fewer than 2 rings or nothing would be inserted — the caller then
    /// runs the ordinary single-ring path.
    fn poly_loop_cut_selection_rings(&mut self, nid: NodeId) -> bool {
        let mesh = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, .. }) => mesh.clone(),
            _ => return false,
        };
        let mut rings = Self::loop_cut_rings_from_selection(&mesh, &self.poly_sel.selected_edges);
        if rings.len() < 2 {
            return false;
        }
        let total = rings.len();
        rings.truncate(MAX_LOOP_CUT_RINGS);
        if total > rings.len() {
            self.toast(&format!(
                "Loop Cut: {} rings selected — cutting the first {}",
                total,
                rings.len()
            ));
        }
        let ts = Self::loop_cut_fractions(self.loop_cut_count.max(1), 0.0, false);
        let curvature = self.loop_cut_effective_curvature();
        // Dry-run on a clone: `transaction()` always pushes, so a selection that inserts nothing
        // must not open history (same honesty as the single-ring commit).
        let mut scratch = (*mesh).clone();
        if !Self::loop_cut_apply_rings(&mut scratch, &rings, &ts, curvature)
            .iter()
            .any(|v| !v.is_empty())
        {
            return false;
        }
        let n = rings.len();
        self.transaction(&format!("Loop Cut ({} rings)", n), |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let m = Arc::make_mut(mesh);
                    Self::loop_cut_apply_rings(m, &rings, &ts, curvature);
                }
            }
            s.poly_request_display_sync(nid);
            s.poly_sel.selected_vertices.clear();
            s.poly_sel.selected_edges.clear();
            s.poly_sel.selected_faces.clear();
            s.poly_invalidate_pick_cache();
            s.mark_dirty(DirtyFlags::RENDER);
        });
        self.toast(&format!("Loop cut applied to {} rings", n));
        true
    }

    /// One-shot Loop Cut on the hovered/selected edge (single centered cut).
    /// If no loop is found but edges are selected, falls back to mid-edge series cuts.
    pub(crate) fn poly_loop_cut(&mut self) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };
        // Prefer hover over leftover result selection (same rule as interactive session).
        let he_id = match self
            .poly_sel
            .hovered_edge
            .or_else(|| self.poly_sel.selected_edges.iter().next().copied())
        {
            Some(e) => e,
            None => return,
        };
        // Selection spanning several rings → cut every one, same rule as the Ctrl+R session.
        // Gated on Selected (tool setting): when off, free single-ring cut only.
        // Hover means "cut THIS ring" and still wins over the selection.
        if self.loop_cut_selected
            && self.poly_sel.hovered_edge.is_none()
            && self.poly_sel.selected_edges.len() >= 2
            && self.poly_loop_cut_selection_rings(nid)
        {
            return;
        }
        if self.poly_loop_cut_commit(nid, he_id, 1) {
            self.toast("Loop cut applied");
            return;
        }
        // No loop — segment-cut the seed alone when hover drove the attempt. Only use a multi-edge
        // selection as the series target when Selected is on and there is no hover (selection-only
        // intent). Selected off always free-cuts the single seed (n-gon walk / segment fallback).
        let edges = if !self.loop_cut_selected
            || self.poly_sel.hovered_edge.is_some()
            || self.poly_sel.selected_edges.is_empty()
        {
            let mut s = std::collections::HashSet::new();
            s.insert(he_id);
            s
        } else {
            self.poly_sel.selected_edges.clone()
        };
        let ts = Self::loop_cut_fractions(1, 0.0, false);
        let will = {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                return;
            };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                return;
            };
            let mut scratch = (**mesh).clone();
            !scratch.edge_series_cut(&edges, &ts).is_empty()
        };
        if !will {
            self.toast("Loop cut: nothing to insert");
            return;
        }
        self.transaction("Edge Series Cut", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let m = Arc::make_mut(mesh);
                    let _ = m.edge_series_cut(&edges, &ts);
                }
            }
            s.poly_request_display_sync(nid);
            s.poly_sel.selected_vertices.clear();
            s.poly_sel.selected_edges.clear();
            s.poly_sel.selected_faces.clear();
            s.poly_invalidate_pick_cache();
            s.mark_dirty(DirtyFlags::RENDER);
        });
        self.toast("Edge cuts applied");
    }

    // ---- Interactive session (Ctrl+R): single state, no place-click ----

    /// Per-frame driver while the Loop Cut tool is active. Auto-enters the session on the first hovered
    /// edge, then FOLLOWS the hovered ring until the user locks it (drag / scroll / S). Called from the
    /// viewport update loop.
    pub(crate) fn poly_loop_cut_session_tick(&mut self, rect: egui::Rect) {
        if self.poly_tool != PolyTool::LoopCut { return; }
        if !self.tool.current.is_poly_loop_cut_slide() {
            // Seed from the hovered edge, or from a selected edge (partial cut) when nothing is hovered.
            let seed = self.poly_sel.hovered_edge
                .or_else(|| self.poly_sel.selected_edges.iter().next().copied());
            if let Some(edge) = seed {
                self.poly_start_loop_cut_session(edge, rect);
            }
            return;
        }
        let (locked, cur_edge) = self.tool.current.as_poly_loop_cut_slide()
            .map(|d| (d.locked, d.start_edge)).unwrap_or((true, 0));
        if !locked {
            if let Some(edge) = self.poly_sel.hovered_edge {
                if edge != cur_edge { self.poly_loop_cut_set_ring(edge, rect); }
            }
        }
    }

    /// Create the session on `edge` (preview at centre). A run of >=2 selected edges becomes the
    /// partial-cut limit **only when Selected is on and the seed is part of that selection**
    /// (intentional partial span). If no strip is found, fall back to **segment mode**: cut the
    /// middle of each selected edge (drag slides placement). Selected OFF = free full ring from
    /// the seed (walks quads and n-gons); selection never forces multi-ring or a span limit.
    ///
    /// Owner 2026-08-07: multi loop-cut then a second cut in the other orientation on a plane
    /// failed because confirm selects the new loop edges, and the next Ctrl+R treated that
    /// selection as a partial-ring limit (and often forced the seed onto the previous loop).
    /// Hovering a perpendicular edge must start a full free cut; leftover result selection is
    /// not a partial intent.
    ///
    /// Owner 2026-08-12: Selected toggle — multi-edge selection used to always force cutting every
    /// ring / limiting the span. Users need free control (including through n-gons) without
    /// clearing selection first.
    fn poly_start_loop_cut_session(&mut self, edge: u32, _rect: egui::Rect) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let original_mesh = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, .. }) => mesh.clone(),
            _ => return,
        };
        if edge as usize >= original_mesh.half_edges.len() { return; }

        let selected = self.poly_sel.selected_edges.clone();
        let hover = self.poly_sel.hovered_edge;
        let use_selected = self.loop_cut_selected;
        // Twin-aware membership (selection stores undirected canonical ids).
        let sel_contains = |he: u32| -> bool {
            if selected.contains(&he) {
                return true;
            }
            let h = HalfEdgeId(he);
            if !original_mesh.is_edge_live(h) {
                return false;
            }
            let twin = original_mesh.he(h).twin;
            twin != core_math::half_edge::NO_TWIN && selected.contains(&twin.0)
        };

        // Seed priority:
        // 1. Hovered edge when present → free full cut (follow hover until drag/scroll locks),
        //    unless Selected is on and the hover sits inside a multi-edge selection (partial/multi).
        // 2. Else multi-selection + Selected → partial/multi seed from selection.
        // 3. Else single selection / passed `edge` (Selected off never uses selection as limit).
        //
        // Never pin the seed to a leftover post-cut selection when the user is hovering a
        // different edge for the next orientation.
        let (start_edge, mut locked, use_sel_limit) = if let Some(h) = hover.filter(|&h| {
            (h as usize) < original_mesh.half_edges.len() && original_mesh.is_edge_live(HalfEdgeId(h))
        }) {
            if use_selected && selected.len() >= 2 && sel_contains(h) {
                // Intentional partial/multi: Selected on + hover inside multi-edge selection.
                (h, true, true)
            } else {
                // Free cut on hover — ignore stale result selection as limit/seed.
                (h, false, false)
            }
        } else if use_selected && selected.len() >= 2 {
            let seed = if sel_contains(edge) {
                edge
            } else {
                *selected.iter().next().unwrap()
            };
            (seed, true, true)
        } else if use_selected && selected.len() == 1 {
            // Single selected edge, no hover: lock to it (but not as a multi partial limit).
            (*selected.iter().next().unwrap(), true, false)
        } else {
            (edge, false, false)
        };
        if start_edge as usize >= original_mesh.half_edges.len() {
            return;
        }

        // MULTI-SELECTION: partition the selection into INDEPENDENT RINGS and cut every one
        // (owner 2026-08-10: "CTRL R DOES NOT SUPPORT MULTI SELECTION"). This used to collapse any
        // multi-edge selection into ONE seed plus one span limit, so a selection touching several
        // rings could never produce more than one cut — and the seed came out of `HashSet::iter()`,
        // so WHICH ring survived was nondeterministic.
        //
        // A selection confined to one ring yields exactly one group, so this generalises the old
        // path instead of replacing it, and `use_sel_limit` still gates it — which keeps the
        // 2026-08-07 rule intact: hovering an edge OUTSIDE the selection is a fresh free cut, and
        // leftover result selection never becomes a limit or an extra ring.
        let mut rings: Vec<PolyLoopCutRing> = if use_sel_limit && selected.len() >= 2 {
            Self::loop_cut_rings_from_selection(&original_mesh, &selected)
        } else {
            Vec::new()
        };
        if rings.is_empty() {
            // One ring, no selection span: hover free cut, or a single selected edge.
            let mut members = std::collections::HashSet::new();
            members.insert(start_edge);
            rings.push(Self::loop_cut_resolve_ring(&original_mesh, start_edge, members));
        }

        // Primary = the ring holding the seed. It owns the slide rail, the preview and R / K, so it
        // moves to index 0 and survives the cap. It also keeps the RAW seed id: canonicalising here
        // would flip the rail's direction, and with it the drag sign, on a hovered edge.
        let seed_canon = original_mesh.canonical_edge(HalfEdgeId(start_edge)).0;
        let pi = rings
            .iter()
            .position(|r| {
                original_mesh.canonical_edge(HalfEdgeId(r.start_edge)).0 == seed_canon
                    || r.limit.as_ref().map_or(false, |l| l.contains(&seed_canon))
            })
            .unwrap_or(0);
        rings.swap(0, pi);
        if original_mesh.canonical_edge(HalfEdgeId(rings[0].start_edge)).0 == seed_canon {
            rings[0].start_edge = start_edge;
        }
        let total = rings.len();
        rings.truncate(MAX_LOOP_CUT_RINGS);
        if total > rings.len() {
            self.toast(&format!(
                "Loop Cut: {} rings selected — cutting the first {}",
                total,
                rings.len()
            ));
        }
        let primary = rings.remove(0);
        let extra_rings = rings;

        let start_edge = primary.start_edge;
        let segment_mode = primary.segment_mode;
        let limit = primary.limit.clone();
        if segment_mode || !extra_rings.is_empty() {
            // Segment mode: stay on this edge set. Multi-ring: hover-follow would retarget only
            // the primary and desync it from the extras.
            locked = true;
        }

        let (ra, rb) = self.poly_edge_world_endpoints(nid, &original_mesh, start_edge);
        let count = self.loop_cut_count.max(1);
        let live = self.loop_cut_live;
        self.start_operation(ActiveTool::PolyLoopCutSlide(PolyLoopCutSlide {
            node_id: nid,
            original_mesh,
            start_edge,
            count,
            factor: 0.0,
            clamp: true,
            paired: false,
            live,
            dragging: false,
            locked,
            limit,
            segment_mode,
            // The engage anchor, live from frame one: the slide now starts on bare-cursor travel, so
            // this can no longer wait for an LMB press to be set (ZERO would read as "already
            // travelled a whole screen" and engage instantly).
            start_mouse: self.viewport.viewport_mouse,
            rail_a_world: ra,
            rail_b_world: rb,
            scroll_accum: 0.0,
            num_buf: String::new(),
            curvature: self.loop_cut_effective_curvature(),
            extra_rings,
        }));
        let ring_count = 1 + self
            .tool
            .current
            .as_poly_loop_cut_slide()
            .map(|d| d.extra_rings.len())
            .unwrap_or(0);
        if ring_count > 1 {
            self.toast(&format!("Loop Cut: {} rings from selection", ring_count));
        } else if segment_mode {
            self.toast("Edge cuts (no loop) — move to place · click/Enter confirm");
        }
        if live {
            self.poly_loop_cut_regenerate();
        }
    }

    /// Re-seed the live session from its current primary edge after a tool-setting change that
    /// alters which rings are cut (Selected on/off). Restores the pre-cut mesh first so a live
    /// multi-ring cut does not leave orphan geometry, then starts a fresh session.
    pub(crate) fn poly_loop_cut_reseed_session(&mut self) {
        let (seed, live, nid, orig) = match self.tool.current.as_poly_loop_cut_slide() {
            Some(d) => (d.start_edge, d.live, d.node_id, d.original_mesh.clone()),
            None => return,
        };
        if live {
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    *mesh = orig;
                }
            }
            self.poly_request_display_sync(nid);
        }
        // Drop the modal so start can rebuild rings under the new Selected policy.
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::LoopCut;
        self.poly_start_loop_cut_session(seed, egui::Rect::NOTHING);
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Switch the (unlocked) session to a newly-hovered ring.
    fn poly_loop_cut_set_ring(&mut self, edge: u32, _rect: egui::Rect) {
        let (nid, orig) = match self.tool.current.as_poly_loop_cut_slide() {
            Some(d) => (d.node_id, d.original_mesh.clone()), None => return,
        };
        if edge as usize >= orig.half_edges.len() { return; }
        let (ra, rb) = self.poly_edge_world_endpoints(nid, &orig, edge);
        let anchor = self.viewport.viewport_mouse;
        if let Some(d) = self.tool.current.as_poly_loop_cut_slide_mut() {
            d.start_edge = edge; d.factor = 0.0; d.rail_a_world = ra; d.rail_b_world = rb;
            // Re-anchor the engage test: travelling BETWEEN rings is ring CHOICE, not a slide. Without
            // this, moving from the auto-seeded ring to the one you actually wanted would cross
            // SLIDE_ENGAGE_PX and lock you onto the wrong ring.
            d.start_mouse = anchor;
        }
        if self.loop_cut_live { self.poly_loop_cut_regenerate(); }
    }

    fn poly_edge_world_endpoints(&self, nid: NodeId, mesh: &core_math::half_edge::HalfEdgeMesh, edge: u32) -> (Vec3, Vec3) {
        let wm = self.scene.scene_graph.world_transform(nid);
        let src = mesh.he_source(HalfEdgeId(edge));
        let dst = mesh.he(HalfEdgeId(edge)).vertex;
        (wm.transform_point3(mesh.vert(src).position), wm.transform_point3(mesh.vert(dst).position))
    }

    /// Engage the slide: lock the ring and anchor the drag at `mp`.
    ///
    /// Called from the per-frame update once the bare cursor has travelled [`SLIDE_ENGAGE_PX`] from
    /// the anchor — NOT from an LMB press (owner 2026-08-11: *"Ctrl+R is clic and drag to start
    /// sliding, make it just drag, so clic will accept"*). The button is what accepts now, so it
    /// cannot also mean "start adjusting".
    pub(crate) fn poly_loop_cut_begin_drag(&mut self, mp: egui::Pos2) {
        if let Some(d) = self.tool.current.as_poly_loop_cut_slide_mut() {
            d.dragging = true; d.locked = true; d.start_mouse = mp;
        }
    }

    /// Update the slide factor from the mouse projected onto the rail's screen axis. Half the rail
    /// length of travel = factor ±1.
    ///
    /// Before the slide engages, this is the ENGAGE test: the ring still follows the hovered edge
    /// (`poly_loop_cut_set_ring` re-anchors `start_mouse` on every retarget), so travel away from the
    /// anchor without a retarget is the user's "I picked this ring, now move it" — at
    /// [`SLIDE_ENGAGE_PX`] it locks and starts sliding from there. Re-anchoring on engage means the
    /// threshold is not paid twice, so the loop does not jump on the first slid frame.
    pub(crate) fn poly_update_loop_cut_slide(&mut self, mp: egui::Pos2, rect: egui::Rect) {
        let (start_mouse, a, b, clamp, prev, dragging) = {
            let d = match self.tool.current.as_poly_loop_cut_slide() { Some(d) => d, None => return };
            (d.start_mouse, d.rail_a_world, d.rail_b_world, d.clamp, d.factor, d.dragging)
        };
        if !dragging {
            if (mp - start_mouse).length() < SLIDE_ENGAGE_PX {
                return;
            }
            self.poly_loop_cut_begin_drag(mp);
            return;
        }
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let sa = world_to_screen(a, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg);
        let sb = world_to_screen(b, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg);
        let factor = match (sa, sb) {
            (Some(sa), Some(sb)) => {
                let (ex, ey) = (sb.x - sa.x, sb.y - sa.y);
                let len = (ex * ex + ey * ey).sqrt();
                if len > 1e-3 {
                    let f = ((mp.x - start_mouse.x) * (ex / len) + (mp.y - start_mouse.y) * (ey / len)) / (len * 0.5);
                    if clamp { f.clamp(-1.0, 1.0) } else { f.clamp(-4.0, 4.0) }
                } else { prev }
            }
            _ => prev,
        };
        if (factor - prev).abs() < 1e-4 { return; }
        if let Some(d) = self.tool.current.as_poly_loop_cut_slide_mut() { d.factor = factor; }
        if self.loop_cut_live { self.poly_loop_cut_regenerate(); }
    }

    /// Re-cut the real mesh (LIVE mode only) from the pre-cut snapshot at the current state.
    pub(crate) fn poly_loop_cut_regenerate(&mut self) {
        let (nid, orig, count, factor, paired, rings, curvature) = {
            let d = match self.tool.current.as_poly_loop_cut_slide() {
                Some(d) => d,
                None => return,
            };
            (
                d.node_id,
                d.original_mesh.clone(),
                d.count,
                d.factor,
                d.paired,
                Self::loop_cut_session_rings(d),
                d.curvature,
            )
        };
        let ts = Self::loop_cut_fractions(count, factor, paired);
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                let mut m = (*orig).clone();
                Self::loop_cut_apply_rings(&mut m, &rings, &ts, curvature);
                *mesh = Arc::new(m);
            }
        }
        self.poly_request_display_sync(nid);
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn poly_loop_cut_scroll_count(&mut self, delta: f32) {
        let changed = if let Some(d) = self.tool.current.as_poly_loop_cut_slide_mut() {
            d.locked = true;
            let before = d.count;
            d.scroll_accum += delta;
            let t = 40.0;
            while d.scroll_accum > t { d.scroll_accum -= t; d.count = (d.count + 1).min(20); }
            while d.scroll_accum < -t { d.scroll_accum += t; d.count = d.count.saturating_sub(1).max(1); }
            d.count != before
        } else { false };
        if changed {
            if let Some(c) = self.tool.current.as_poly_loop_cut_slide().map(|d| d.count) { self.loop_cut_count = c; }
            if self.loop_cut_live { self.poly_loop_cut_regenerate(); }
        }
    }

    pub(crate) fn poly_loop_cut_toggle_clamp(&mut self) {
        let s = if let Some(d) = self.tool.current.as_poly_loop_cut_slide_mut() { d.clamp = !d.clamp; Some(d.clamp) } else { None };
        if let Some(on) = s {
            if self.loop_cut_live { self.poly_loop_cut_regenerate(); }
            self.toast(if on { "Loop Cut: Clamp ON" } else { "Loop Cut: Clamp OFF" });
        }
    }

    pub(crate) fn poly_loop_cut_toggle_paired(&mut self) {
        let s = if let Some(d) = self.tool.current.as_poly_loop_cut_slide_mut() { d.paired = !d.paired; d.locked = true; Some(d.paired) } else { None };
        if let Some(on) = s {
            if self.loop_cut_live { self.poly_loop_cut_regenerate(); }
            self.toast(if on { "Loop Cut: paired support loops ON (S)" } else { "Loop Cut: paired OFF" });
        }
    }

    /// Confirm — bake the cut (ONE undo). RMB/Enter. Rebuilds from the pre-cut snapshot so the undo
    /// before-image is clean whether we were in preview or live mode.
    ///
    /// If the cut inserts nothing (degenerate edge / failed multi-cut), restore and exit with
    /// **no** undo entry (fill/bridge/slide honesty).
    pub(crate) fn poly_confirm_loop_cut_slide(&mut self) {
        let (nid, orig, count, factor, paired, rings, segment_mode, curvature) = {
            let d = match self.tool.current.as_poly_loop_cut_slide() {
                Some(d) => d,
                None => return,
            };
            (
                d.node_id,
                d.original_mesh.clone(),
                d.count,
                d.factor,
                d.paired,
                Self::loop_cut_session_rings(d),
                d.segment_mode,
                d.curvature,
            )
        };
        let ts = Self::loop_cut_fractions(count, factor, paired);
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        // Restore the pre-cut base so the transaction snapshots a clean before-image (live mode
        // mutated the mesh outside undo; preview mode restore is a no-op).
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                *mesh = orig.clone();
            }
        }
        // Dry-run: refuse-before-transaction if nothing would be inserted (any ring counts).
        let will_insert = {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                return;
            };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                return;
            };
            let mut scratch = (**mesh).clone();
            Self::loop_cut_apply_rings(&mut scratch, &rings, &ts, curvature)
                .iter()
                .any(|v| !v.is_empty())
        };
        if !will_insert {
            self.poly_request_display_sync(nid);
            self.poly_invalidate_pick_cache();
            self.mark_dirty(DirtyFlags::RENDER);
            self.toast("Loop cut: nothing to insert");
            return;
        }
        let undo_label = if segment_mode {
            "Edge Series Cut"
        } else {
            "Loop Cut"
        };
        self.transaction(undo_label, |s| {
            let mut per_ring: Vec<Vec<u32>> = Vec::new();
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let m = Arc::make_mut(mesh);
                    per_ring = Self::loop_cut_apply_rings(m, &rings, &ts, curvature);
                }
            }
            s.poly_request_display_sync(nid);
            s.poly_sel.selected_vertices.clear();
            s.poly_sel.selected_edges.clear();
            s.poly_sel.selected_faces.clear();
            // SELECT THE RESULT — a cut you cannot immediately act on is a dead end.
            //
            // Owner 2026-07-30: "Ctrl+R creates preview, user presses G it confirms the cut and
            // selects result." This used to clear the selection entirely, so after a loop cut the
            // user had to re-pick the loop they had just created before they could slide or grab it.
            // Blender leaves the new loop selected, which is what makes Ctrl+R → G a fluid pair.
            //
            // Result EDGES = half-edges whose both endpoints are new loop verts AND that run
            // across the strip (perpendicular to the seed rail). Multi-cut also creates short
            // rail chords between consecutive insert points on the SAME rail — both ends are new
            // verts, but those edges are NOT the cut loops. Selecting them poisoned the next
            // Ctrl+R (seed/limit stuck on the previous orientation) — owner 2026-08-07 plane
            // multi U then multi V. Verts are always selected for G / transform.
            //
            // MULTI-RING: the perpendicular test is PER RING, keyed by which ring inserted each
            // vertex. One global seed direction would be wrong as soon as the rings differ in
            // orientation — 16 meridian rings around a hole have seed rails pointing every which
            // way, so ring A's real cut loop is parallel to ring B's seed rail and would be
            // dropped. An edge only qualifies when BOTH endpoints came from the SAME ring.
            let total_new: usize = per_ring.iter().map(|v| v.len()).sum();
            if total_new > 0 {
                let mut vert_ring: std::collections::HashMap<u32, usize> =
                    std::collections::HashMap::with_capacity(total_new);
                for (ri, verts) in per_ring.iter().enumerate() {
                    for &v in verts {
                        vert_ring.insert(v, ri);
                    }
                }
                // Per-ring seed rail direction, from the pre-cut snapshot.
                let seed_dirs: Vec<glam::Vec3> = rings
                    .iter()
                    .map(|r| {
                        let he = HalfEdgeId(r.start_edge);
                        if (r.start_edge as usize) < orig.half_edges.len() && orig.is_edge_live(he) {
                            let a = orig.vert(orig.he_source(he)).position;
                            let b = orig.vert(orig.he(he).vertex).position;
                            (b - a).normalize_or_zero()
                        } else {
                            glam::Vec3::ZERO
                        }
                    })
                    .collect();
                if let Some(node) = s.scene.scene_graph.get(nid) {
                    if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
                        for i in 0..mesh.half_edges.len() as u32 {
                            let he = HalfEdgeId(i);
                            if !mesh.is_edge_live(he) { continue; }
                            let d = mesh.he(he).vertex.0;
                            let src = mesh.he_source(he).0;
                            let (Some(&rd), Some(&rs)) = (vert_ring.get(&d), vert_ring.get(&src))
                            else {
                                continue;
                            };
                            if rd != rs { continue; }
                            // Drop same-rail chords (parallel to seed); keep cut-loop edges.
                            let seed_dir = seed_dirs[rd];
                            if seed_dir.length_squared() > 1e-8 {
                                let pa = mesh.vert(mesh.he_source(he)).position;
                                let pb = mesh.vert(mesh.he(he).vertex).position;
                                let edir = (pb - pa).normalize_or_zero();
                                // |dot| > cos(45°) → mostly parallel to seed rail → not a cut loop.
                                if edir.dot(seed_dir).abs() > 0.707 {
                                    continue;
                                }
                            }
                            let twin = mesh.he(he).twin;
                            let canon = if twin != core_math::half_edge::NO_TWIN {
                                i.min(twin.0)
                            } else {
                                i
                            };
                            if seen.insert(canon) {
                                s.poly_sel.selected_edges.insert(canon);
                            }
                        }
                    }
                }
                for verts in &per_ring {
                    for &v in verts {
                        s.poly_sel.selected_vertices.insert(v);
                    }
                }
            }
            s.poly_invalidate_pick_cache();
            s.mark_dirty(DirtyFlags::RENDER);
        });
        // NO AUTO-GROUP AFTER A LOOP CUT.
        //
        // Owner 2026-07-30: "at some point the selection tool did not work after I went in polygroup
        // selection mode, right after the loop cut."
        //
        // This used to call `poly_auto_group_all_faces`, which puts EVERY live face into ONE group.
        // In Group mode a click selects the hovered face's whole group — so after any loop cut,
        // clicking anywhere selected the ENTIRE MESH. The selection tool was not broken; it was
        // faithfully selecting a group that contained everything.
        //
        // The intent was sound (loop cut rebuilds via `from_polygons`, so face ids are remapped and
        // provenance is lost — bookmark something so the user can reselect). But a group of ALL
        // faces carries no information: it cannot distinguish the cut from the rest of the model,
        // which is the one thing a polygroup is for. Per AUTO_POLYGROUPS.md the rule is one group per
        // MEANINGFUL action, and "everything" is not meaningful.
        //
        // The real fix is to tag only the faces the cut CREATED, which needs `loop_cut_multi` to
        // report them (it currently returns only VertexIds). Until then, no group is strictly better
        // than a wrong one — the confirm path already selects the new loop, which is what the user
        // actually needs immediately after cutting.
        self.toast(if segment_mode {
            "Edge cuts applied"
        } else {
            "Loop cut applied"
        });
    }

    /// Cancel — restore the pre-cut mesh (if live changed it) and exit. Esc.
    pub(crate) fn poly_cancel_loop_cut_slide(&mut self) {
        let (nid, orig, live) = match self.tool.current.as_poly_loop_cut_slide() {
            Some(d) => (d.node_id, d.original_mesh.clone(), d.live),
            None => { self.tool.current = ActiveTool::Idle; self.poly_tool = PolyTool::Idle; return; }
        };
        if live {
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload { *mesh = orig; }
            }
            self.poly_request_display_sync(nid);
        }
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        self.poly_invalidate_pick_cache();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// True while Ctrl+R loop cut owns the session (armed tool and/or live slide modal).
    pub(crate) fn poly_loop_cut_is_active(&self) -> bool {
        self.poly_tool == PolyTool::LoopCut || self.tool.current.is_poly_loop_cut_slide()
    }

    /// Accept a live loop-cut session (or just drop the armed tool). Used when Tab / 1–3 switch
    /// selection mode mid-tool — owner 2026-08-06: changing selection mode must accept the cut.
    /// Returns true if loop cut was active and was handled.
    pub(crate) fn poly_loop_cut_accept_on_mode_switch(&mut self) -> bool {
        if self.tool.current.is_poly_loop_cut_slide() {
            self.poly_confirm_loop_cut_slide();
            return true;
        }
        if self.poly_tool == PolyTool::LoopCut {
            self.poly_tool = PolyTool::Idle;
            return true;
        }
        false
    }

    /// R while loop cut is live: rotate the cut direction 90° on the seed face.
    ///
    /// The loop-cut seed edge defines which strip of quads is crossed. Stepping to the next
    /// half-edge around that face re-aims the ring so the cut runs the other way across the
    /// connected segments (vertical ↔ horizontal on a cube face).
    pub(crate) fn poly_loop_cut_rotate_direction(&mut self) {
        let (edge, nid, orig) = match self.tool.current.as_poly_loop_cut_slide() {
            Some(d) => (d.start_edge, d.node_id, d.original_mesh.clone()),
            None => {
                // Armed but not yet seeded: flip from hover / selection if available.
                if self.poly_tool != PolyTool::LoopCut {
                    return;
                }
                let seed = self
                    .poly_sel
                    .hovered_edge
                    .or_else(|| self.poly_sel.selected_edges.iter().next().copied());
                let Some(e) = seed else {
                    self.toast("Loop Cut: hover an edge, then R to rotate direction");
                    return;
                };
                let Some(nid) = self.sel.usel.active_node() else { return };
                let Some(node) = self.scene.scene_graph.get(nid) else { return };
                let NodePayload::PolygonalBody { mesh, .. } = &node.payload else { return };
                let next = Self::loop_cut_next_face_edge(mesh, e).unwrap_or(e);
                self.poly_sel.hovered_edge = Some(next);
                self.toast("Loop Cut: direction rotated (R)");
                return;
            }
        };
        let next = match Self::loop_cut_next_face_edge(&orig, edge) {
            Some(n) if n != edge => n,
            _ => {
                self.toast("Loop Cut: cannot rotate on this edge");
                return;
            }
        };
        // Unlock so set_ring can retarget; then lock again if we were locked (user intent).
        // Drop any partial-cut limit: R aims a new orientation; an old limit (often leftover
        // from the previous cut's result selection) would empty the ring and force segment mode.
        let was_locked = self
            .tool
            .current
            .as_poly_loop_cut_slide()
            .map(|d| d.locked)
            .unwrap_or(false);
        if let Some(d) = self.tool.current.as_poly_loop_cut_slide_mut() {
            d.locked = false;
            d.segment_mode = false;
            d.limit = None;
        }
        self.poly_loop_cut_set_ring(next, egui::Rect::NOTHING);
        // Re-check whether the new seed has a real loop; fall back to segment if not.
        if let Some(d) = self.tool.current.as_poly_loop_cut_slide_mut() {
            let has_loop = d
                .original_mesh
                .loop_cut_ring_has_quads(HalfEdgeId(d.start_edge), None);
            d.segment_mode = !has_loop;
            if has_loop {
                d.limit = None;
            } else {
                let mut s = std::collections::HashSet::new();
                s.insert(d.start_edge);
                d.limit = Some(s);
            }
            d.locked = was_locked || d.segment_mode;
            d.factor = 0.0;
        }
        // R re-aims EVERY ring in the session, not just the primary — leaving the extras on their
        // old seeds would cut two different orientations at once. Rotated seeds can collapse onto
        // the SAME ring (16 meridians around a hole all rotate onto the one circumferential ring),
        // so claim each new ring's rails and skip seeds already covered; without that dedup one
        // ring would be cut N times, stacking N loops in the same place.
        let extras_before = self
            .tool
            .current
            .as_poly_loop_cut_slide()
            .map(|d| d.extra_rings.clone())
            .unwrap_or_default();
        if !extras_before.is_empty() {
            let mut claimed: std::collections::HashSet<u32> = std::collections::HashSet::new();
            let primary_seed = self
                .tool
                .current
                .as_poly_loop_cut_slide()
                .map(|d| d.start_edge)
                .unwrap_or(next);
            if orig.is_edge_live(HalfEdgeId(primary_seed)) {
                claimed.insert(orig.canonical_edge(HalfEdgeId(primary_seed)).0);
            }
            claimed.extend(orig.loop_cut_ring_rail_edges(HalfEdgeId(primary_seed), None));
            let mut rotated: Vec<PolyLoopCutRing> = Vec::with_capacity(extras_before.len());
            for r in &extras_before {
                let Some(n) = Self::loop_cut_next_face_edge(&orig, r.start_edge) else { continue };
                if !orig.is_edge_live(HalfEdgeId(n)) { continue; }
                if !claimed.insert(orig.canonical_edge(HalfEdgeId(n)).0) { continue; }
                claimed.extend(orig.loop_cut_ring_rail_edges(HalfEdgeId(n), None));
                // Limits are dropped for the same reason as the primary's: an old span belongs to
                // the previous orientation and would empty the re-aimed ring.
                let mut members = std::collections::HashSet::new();
                members.insert(n);
                rotated.push(Self::loop_cut_resolve_ring(&orig, n, members));
            }
            if let Some(d) = self.tool.current.as_poly_loop_cut_slide_mut() {
                d.extra_rings = rotated;
            }
        }
        let _ = nid;
        if self.loop_cut_live {
            self.poly_loop_cut_regenerate();
        }
        self.toast("Loop Cut: direction rotated (R)");
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Next half-edge around the seed's face (90° on a quad). Falls back to twin's next.
    fn loop_cut_next_face_edge(mesh: &core_math::half_edge::HalfEdgeMesh, edge: u32) -> Option<u32> {
        use core_math::half_edge::{HalfEdgeId, NO_EDGE, NO_FACE};
        let he = HalfEdgeId(edge);
        if !mesh.is_edge_live(he) {
            return None;
        }
        let face = mesh.he(he).face;
        if face != NO_FACE && (face.0 as usize) < mesh.faces.len() {
            let next = mesh.he(he).next;
            if next != NO_EDGE && mesh.is_edge_live(next) && next.0 != edge {
                return Some(next.0);
            }
        }
        // Boundary / no face: try the twin's face walk.
        let twin = mesh.he(he).twin;
        if twin != core_math::half_edge::NO_TWIN && mesh.is_edge_live(twin) {
            let next = mesh.he(twin).next;
            if next != NO_EDGE && mesh.is_edge_live(next) {
                return Some(next.0);
            }
        }
        None
    }

    /// K while loop cut is live: **one-shot through-slice** along the preview ring, through every
    /// mesh island the path crosses.
    ///
    /// Owner 2026-08-06: must **NOT** arm the real Knife tool (`poly_tool = Knife` / knife modal).
    /// Previous path cancelled Ctrl+R, set `PolyTool::Knife`, then sliced — leaving the knife
    /// armed and dropping loop cut. This path:
    /// 1. builds a clean closed polyline from the loop-cut rails,
    /// 2. restores the pre-cut mesh and ends the loop-cut session,
    /// 3. runs free-plane through-slice with temporary knife state,
    /// 4. clears all knife state and leaves tools **Idle**.
    pub(crate) fn poly_loop_cut_project_knife_through(&mut self) {
        let (nid, orig, count, factor, paired, rings, curvature) =
            match self.tool.current.as_poly_loop_cut_slide() {
                Some(d) => (
                    d.node_id,
                    d.original_mesh.clone(),
                    d.count,
                    d.factor,
                    d.paired,
                    Self::loop_cut_session_rings(d),
                    d.curvature,
                ),
                None => {
                    self.toast("Loop Cut: hover a ring first, then K for through-cut");
                    return;
                }
            };
        let fractions = Self::loop_cut_fractions(count.max(1), factor, paired);
        // One through-path per cut fraction (multi-cut scroll).
        let world = self.scene.scene_graph.world_transform(nid);
        let to_world = |p: glam::Vec3| world.transform_point3(p);

        // One through-path per cut fraction PER RING — a multi-ring session slices every ring.
        let mut paths: Vec<Vec<glam::Vec3>> = Vec::new();
        for ring in &rings {
            for &t in &fractions {
                let mut pts: Vec<glam::Vec3> = Vec::new();
                if ring.segment_mode {
                    let edges = Self::loop_cut_segment_edges(ring);
                    let mut seen = std::collections::HashSet::new();
                    for &eid in &edges {
                        let he = HalfEdgeId(eid);
                        if !orig.is_edge_live(he) {
                            continue;
                        }
                        if !seen.insert(orig.canonical_edge(he).0) {
                            continue;
                        }
                        let s = orig.he_source(he);
                        let d = orig.he(he).vertex;
                        let mid = orig.vert(s).position.lerp(orig.vert(d).position, t);
                        pts.push(to_world(mid));
                    }
                } else {
                    // One point per rail (entry side) — closed ring, not zigzag entry+exit pairs.
                    // Same `rail_curve_point` as the cut, so a follow-curvature through-cut runs
                    // where the loop would have landed instead of on the chords.
                    let rails = orig
                        .loop_cut_preview_rail_verts(HalfEdgeId(ring.start_edge), ring.limit.as_ref());
                    for &((eb, et), _) in &rails {
                        let a = orig.rail_curve_point(eb, et, t, curvature);
                        let w = to_world(a);
                        if pts
                            .last()
                            .map_or(true, |q| (*q - w).length_squared() > 1e-12)
                        {
                            pts.push(w);
                        }
                    }
                }
                if pts.len() >= 2 {
                    // Close the loop for through-cut (first == last).
                    let first = pts[0];
                    let last = *pts.last().unwrap();
                    if (first - last).length() > 1e-4 {
                        pts.push(first);
                    }
                    paths.push(pts);
                }
            }
        }
        if paths.is_empty() {
            self.toast("Loop Cut through-cut: need a longer path");
            return;
        }

        // End loop-cut session first (restores pre-cut mesh; clears poly_tool / ActiveTool).
        self.poly_cancel_loop_cut_slide();

        // View-aligned free plane through path centroid (same freeze as free-knife R1).
        let mut c = glam::Vec3::ZERO;
        let mut n_pts = 0usize;
        for path in &paths {
            for p in path {
                c += *p;
                n_pts += 1;
            }
        }
        c /= n_pts.max(1) as f32;
        let orbit = self.viewport.camera.orbit;
        let plane = (
            c,
            (orbit * glam::Vec3::X).normalize(),
            (orbit * glam::Vec3::Y).normalize(),
            (orbit * glam::Vec3::NEG_Z).normalize(),
        );

        // Temporary knife state ONLY for the slice engine — never leave PolyTool::Knife armed.
        self.knife_shape_kind = 0;
        self.knife_shape_anchor = None;
        self.knife_shape_cursor = None;
        self.knife_shape_screen_a = None;
        self.knife_shape_screen_b = None;
        self.knife_snap_pos = None;
        self.knife_free = true;
        self.knife_free_slice = true;
        self.knife_free_plane = Some(plane);

        let mut any = false;
        for path in paths {
            self.knife_points = path;
            // free_slice_commit does not require poly_tool == Knife (slice_through_now does).
            self.poly_knife_free_slice_commit(true, true, crate::poly_knife::SliceEnclosed::Keep);
            any = true;
            // free_slice_commit clears knife_points / free plane on success — re-arm plane for
            // multi-cut paths on the same mesh.
            self.knife_free = true;
            self.knife_free_slice = true;
            self.knife_free_plane = Some(plane);
        }

        // Hard-clear knife modality so K never leaves the real knife tool armed.
        self.knife_points.clear();
        self.knife_snap_pos = None;
        self.knife_free = false;
        self.knife_free_slice = false;
        self.knife_free_plane = None;
        self.knife_shape_kind = 0;
        self.knife_shape_anchor = None;
        self.knife_shape_cursor = None;
        self.poly_tool = PolyTool::Idle;
        self.tool.current = ActiveTool::Idle;

        let _ = nid;
        if any {
            self.toast("Loop Cut: through-cut applied (K) — knife tool not armed");
        } else {
            self.toast("Loop Cut: through-cut found nothing to cut");
        }
    }
}
