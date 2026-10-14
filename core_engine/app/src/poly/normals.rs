use std::collections::HashSet;
use std::sync::Arc;
use core_math::half_edge::{
    FaceId, HalfEdgeId, HalfEdgeMesh, smooth_corner_normals_ex, weighted_corner_normals,
};
use core_math::poly_mesh::{BitLayer, NormalConfig};
use core_math::scene_graph::{NodeId, NodePayload, PolyModifierType};
use crate::{AzraApp, DirtyFlags};

/// Owned form of [`NormalConfig`] for one body's DISPLAY mesh.
///
/// The authored `sharp` column keys on the BASE mesh's half-edge ids, but the bake runs on the
/// post-modifier display mesh, whose ids may be renumbered. So the set is remapped onto the
/// display mesh here (by position, via the canonical `CustomData::remap_by_position`) and the
/// result must be OWNED — hence this type rather than a borrowed `NormalConfig` straight from
/// the sidecar. Unmatched display geometry (a Mirror's mirrored half, a Subdivide's new edges)
/// gets no sharp bit: fail-closed, a mark is never GUESSED onto geometry it wasn't authored on.
///
/// **Weighted / Smooth Normal modifiers** produce `custom_corner` (per half-edge). Decisions:
/// - **Stack order:** the **LAST enabled** WeightedNormal / SmoothNormal wins (natural stack read).
/// - **Mark Sharp:** still splits — passed into the corner producer as islands. Auto Smooth's
///   angle fan is **replaced** entirely when `custom_corner` is set (not layered on top).
pub(crate) struct PolyNormalState {
    pub angle_deg: f32,
    pub sharp: Option<BitLayer<HalfEdgeId>>,
    /// Per-half-edge corner normals from the last enabled Weighted/Smooth Normal, if any.
    pub custom_corner: Option<Vec<glam::Vec3>>,
}

impl PolyNormalState {
    /// Borrowed config for the kernel emitter.
    pub(crate) fn cfg(&self) -> NormalConfig<'_> {
        NormalConfig {
            angle_deg: self.angle_deg,
            sharp: self.sharp.as_ref(),
            custom_corner: self.custom_corner.as_deref(),
        }
    }
}

impl Default for PolyNormalState {
    fn default() -> Self {
        Self {
            angle_deg: core_math::poly_mesh::AUTO_SMOOTH_ANGLE_DEG,
            sharp: None,
            custom_corner: None,
        }
    }
}

impl AzraApp {
    /// The normal-authoring state to bake `display` (the post-modifier display mesh) of body `nid`
    /// with. The ONE place the sidecar **and** normal modifiers become bake inputs — both
    /// `poly_sync_mesh_cache` and `poly_sync_after_position_edit` go through it, so display can
    /// never disagree with itself (or with export, which also calls this).
    pub(crate) fn poly_normal_state(
        &self,
        nid: NodeId,
        display: &HalfEdgeMesh,
    ) -> PolyNormalState {
        let (angle_deg, sharp_opt, mods) = {
            let cd = self.cache.poly_custom_data.get(&nid);
            let angle_deg = cd
                .map(|c| c.auto_smooth_angle())
                .unwrap_or(core_math::poly_mesh::AUTO_SMOOTH_ANGLE_DEG);
            let base = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
                NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
                _ => None,
            });
            let sharp = match (cd, base.as_ref()) {
                (Some(cd), _) if cd.sharp.is_empty() => None,
                (Some(cd), Some(b)) if b.content_signature() == display.content_signature() => {
                    Some(cd.sharp.clone())
                }
                (Some(cd), Some(b)) => Some(cd.remap_by_position(b, display).sharp),
                (Some(cd), None) => Some(cd.sharp.clone()),
                (None, _) => None,
            };
            let mods = self
                .scene
                .scene_graph
                .get(nid)
                .and_then(|n| match &n.payload {
                    NodePayload::PolygonalBody { modifiers, .. } => Some(modifiers.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            (angle_deg, sharp, mods)
        };

        // LAST enabled WeightedNormal / SmoothNormal wins (stack evaluation order).
        let last_nmod = mods.iter().rev().find(|m| {
            m.enabled
                && matches!(
                    m.mod_type,
                    PolyModifierType::WeightedNormal | PolyModifierType::SmoothNormal
                )
        });

        let custom_corner = last_nmod.map(|m| {
            let sharp_set: Option<HashSet<u32>> = sharp_opt.as_ref().map(|layer| {
                (0..display.half_edges.len() as u32)
                    .filter(|&i| {
                        let he = HalfEdgeId(i);
                        display.is_edge_live(he) && layer.get(he)
                    })
                    .collect()
            });
            let sharp_ref = sharp_set.as_ref();
            match m.mod_type {
                PolyModifierType::WeightedNormal => weighted_corner_normals(
                    display,
                    m.weighted_keep_sharp_deg,
                    m.weighted_mode,
                    m.weighted_weight,
                    m.weighted_threshold,
                    sharp_ref,
                ),
                PolyModifierType::SmoothNormal => {
                    // Same knobs as Weighted + Strength (smooth_factor). Last enabled wins.
                    let legacy = (m.weighted_keep_sharp_deg - 30.0).abs() < 1e-3
                        && (m.smooth_factor - 0.5).abs() < 1e-3
                        && m.weighted_weight == 50
                        && m.weighted_mode == 2
                        && (m.weighted_threshold - 0.01).abs() < 1e-4;
                    let angle = if legacy { 180.0 } else { m.weighted_keep_sharp_deg };
                    let strength = if legacy { 1.0 } else { m.smooth_factor };
                    smooth_corner_normals_ex(
                        display,
                        angle,
                        m.weighted_mode,
                        m.weighted_weight,
                        m.weighted_threshold,
                        strength,
                        sharp_ref,
                    )
                }
                _ => unreachable!("filtered above"),
            }
        });

        PolyNormalState {
            angle_deg,
            sharp: sharp_opt,
            custom_corner,
        }
    }

    /// Digest of body `nid`'s authored normal state, folded into the display cache key so a
    /// Mark Sharp / angle edit INVALIDATES the cached GPU mesh. Without this the key would be
    /// `(mesh signature, modifier hash)` — neither of which a normal edit touches — and the stale
    /// bake would be reused (the exact class of bug the Smart Select tags hit, mesh_sync.rs:508).
    /// `None` when nothing is authored, so an untouched body pays nothing.
    ///
    /// Weighted/Smooth Normal params are already in `poly_modifier_stack_hash` — not duplicated here.
    pub(crate) fn poly_normal_key(&self, nid: NodeId, base: &HalfEdgeMesh) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let cd = self.cache.poly_custom_data.get(&nid)?;
        if !cd.has_authored_normals() {
            return None;
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        cd.auto_smooth_angle().to_bits().hash(&mut h);
        let mut ids: Vec<u32> = (0..base.half_edges.len() as u32)
            .filter(|&i| {
                let he = HalfEdgeId(i);
                base.is_edge_live(he) && cd.sharp.get(he)
            })
            .collect();
        ids.sort_unstable();
        ids.hash(&mut h);
        Some(h.finish())
    }
}

impl AzraApp {
    /// Recalculate all normals to point outward (Shift+N) — Blender parity.
    ///
    /// This used to call `recompute_normals()` alone, which derives each face's normal FROM its
    /// winding. On a mesh with inconsistently wound faces — the only mesh anyone ever presses
    /// Shift+N on — that produces a correctly computed WRONG-WAY normal, i.e. it changed nothing
    /// visible (`UNT_SUITE_REPORT.md` recorded it as a no-op after a flip). Winding is what
    /// Shift+N exists to fix, so it now goes through
    /// [`HalfEdgeMesh::recalculate_normals_consistent`]: consistent winding across every shell,
    /// oriented outward on the closed ones (an open shell has no "outside" to orient to, so it is
    /// only made self-consistent). `recompute_normals` still runs, inside that op.
    ///
    /// Refuse before `transaction()` when the active node is not a poly mesh — otherwise
    /// `transaction` always pushes an undo entry even when the closure does nothing
    /// (same honesty class as fill/bridge/inset refuse-before-tx).
    pub(crate) fn poly_recalculate_normals(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let is_poly = matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::PolygonalBody { .. })
        );
        if !is_poly {
            self.toast("Recalculate Normals: active object is not a poly mesh");
            return;
        }
        self.transaction("Recalculate Normals", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let mesh = Arc::make_mut(mesh);
                    let flipped = mesh.recalculate_normals_consistent(true);
                    s.poly_request_display_sync(nid);
                    s.mark_dirty(DirtyFlags::RENDER);
                    // Report the real count — "Normals recalculated" on a mesh where nothing was
                    // wrong reads as a fix that did not happen.
                    if flipped > 0 {
                        s.toast(&format!("Normals recalculated — {flipped} face(s) re-wound"));
                    } else {
                        s.toast("Normals recalculated — winding was already consistent");
                    }
                }
            }
        });
    }

    /// Flip normals of selected faces (reverses winding order) — delta undo (§4.6, Phase 2).
    /// flip_face is in-place (no add, no free) → O(touched) ElementDelta.
    pub(crate) fn poly_flip_normals(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        // Modo implicit full selection: empty selection flips EVERY face (SAFE — reverses winding, no
        // geometry change) via the shared `poly_faces_or_all` resolver.
        let faces: Vec<u32> = self.poly_faces_or_all(nid);
        if faces.is_empty() { return; }
        // Flip keeps every face selected (applied AND refused) — so `keep` is the whole set and the
        // reporter is a no-op on selection, exactly as before. The old toast printed `faces.len()`,
        // the REQUESTED count, as if it were the applied count; the outcome carries the real one.
        let ops: Vec<core_math::mesh_ops::PolyEdit> =
            faces.iter().map(|&f| core_math::mesh_ops::PolyEdit::FlipFace { face: f }).collect();
        let out = self.poly_op_batch(nid, ops, "Flip Normals");
        self.poly_report_batch(&out, "flipped", &faces);
    }

    // =================================================================================
    // Blender-style normal AUTHORING: Mark Sharp / Clear Sharp / Shade Smooth / Shade Flat
    //
    // Two independent controls, exactly like Blender:
    //   * per-edge `sharp` bits   → a hard edge that survives any Auto Smooth angle
    //   * per-body Auto Smooth °  → 0 = Shade Flat, 180 = Shade Smooth, anything between =
    //                               "Smooth by Angle"
    // Both live on the runtime `CustomData` sidecar (never on the serialized mesh — save-format
    // red line) and both are snapshotted by `transaction()` (see `undo.rs`).
    // =================================================================================

    /// The active poly body, or a refusal toast. Refusing BEFORE `transaction()` matters:
    /// `transaction` pushes an undo entry unconditionally, so a no-op closure would leave a
    /// phantom Ctrl+Z step (same honesty rule as `poly_recalculate_normals`).
    fn poly_normals_target(&mut self, what: &str) -> Option<NodeId> {
        let nid = self.sel.usel.active_node()?;
        if !matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::PolygonalBody { .. })
        ) {
            self.toast(&format!("{what}: active object is not a poly mesh"));
            return None;
        }
        Some(nid)
    }

    /// Mark (or clear) the selected edges as SHARP — Blender's Mark Sharp / Clear Sharp.
    ///
    /// A sharp edge breaks the smooth-normal fan whatever the Auto Smooth angle is. BOTH
    /// half-edges of each selected edge are written, so the mark is symmetric on disk and cannot
    /// depend on which side the user happened to click (the reader checks both anyway).
    pub(crate) fn poly_set_sharp(&mut self, on: bool) {
        let what = if on { "Mark Sharp" } else { "Clear Sharp" };
        let Some(nid) = self.poly_normals_target(what) else { return };
        let edges: Vec<u32> = self.poly_sel.selected_edges.iter().copied().collect();
        if edges.is_empty() {
            self.toast(&format!("{what}: select edges first"));
            return;
        }
        // Resolve each selected half-edge to its twin up front (immutable graph read), so the
        // transaction closure only writes.
        let pairs: Vec<(HalfEdgeId, HalfEdgeId)> = match self
            .scene
            .scene_graph
            .get(nid)
            .and_then(|n| match &n.payload {
                NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
                _ => None,
            }) {
            Some(m) => edges
                .iter()
                .map(|&e| HalfEdgeId(e))
                .filter(|&h| m.is_edge_live(h))
                .map(|h| (h, m.he(h).twin))
                .collect(),
            None => return,
        };
        if pairs.is_empty() {
            self.toast(&format!("{what}: selection has no live edges"));
            return;
        }
        let n = pairs.len();
        self.transaction(what, |s| {
            {
                let cd = s.poly_cd_mut(nid);
                for (h, twin) in &pairs {
                    cd.sharp.set(*h, on);
                    if *twin != core_math::half_edge::NO_TWIN {
                        cd.sharp.set(*twin, on);
                    }
                }
            }
            // Edge-mark overlay fingerprint must rebuild (CustomData is outside mesh Arc).
            s.overlay_lines_fp = u64::MAX;
            s.poly_force_display_resync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
        });
        self.toast(&format!("{what}: {n} edge{}", if n == 1 { "" } else { "s" }));
    }

    /// Set the body's Auto Smooth angle (degrees). `0` = Shade Flat, `180` = Shade Smooth.
    /// The ONE writer — Shade Smooth / Shade Flat / the properties slider all route here, so the
    /// three can never disagree about what "smooth" means.
    pub(crate) fn poly_set_auto_smooth_angle(&mut self, deg: f32, label: &str) {
        let Some(nid) = self.poly_normals_target(label) else { return };
        let deg = deg.clamp(0.0, 180.0);
        if self
            .poly_cd(nid)
            .map(|cd| (cd.auto_smooth_angle() - deg).abs() < 1e-4)
            .unwrap_or(false)
        {
            return; // already there — no phantom undo step
        }
        self.transaction(label, |s| s.poly_write_auto_smooth_angle(nid, Some(deg)));
        self.toast(&format!("{label} ({deg:.0}\u{b0})"));
    }

    /// RAW writer — no undo entry of its own. Only for callers that already own the undo step:
    /// [`Self::poly_set_auto_smooth_angle`] (inside `transaction`) and the properties slider
    /// (inside a `begin_episode`/`commit_episode` drag, one entry per drag). Never call it bare.
    pub(crate) fn poly_write_auto_smooth_angle(&mut self, nid: NodeId, deg: Option<f32>) {
        self.poly_cd_mut(nid).auto_smooth_deg = deg.map(|d| d.clamp(0.0, 180.0));
        self.poly_force_display_resync(nid);
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Shade Smooth — smooth across every edge regardless of angle.
    ///
    /// DELIBERATELY does NOT clear the authored `sharp` marks (Blender parity: Shade Smooth and
    /// Mark Sharp are independent, and a Shade Smooth that silently destroyed authored hard edges
    /// would be data loss). Use Clear Sharp to drop the marks.
    pub(crate) fn poly_shade_smooth(&mut self) {
        self.poly_set_auto_smooth_angle(180.0, "Shade Smooth");
    }

    /// Shade Flat — every face keeps its own normal (Auto Smooth 0°).
    pub(crate) fn poly_shade_flat(&mut self) {
        self.poly_set_auto_smooth_angle(0.0, "Shade Flat");
    }

    /// Reset to the app default Auto Smooth angle (drops the authored override entirely, so the
    /// body goes back to being byte-identical to an untouched one).
    pub(crate) fn poly_shade_auto_smooth_default(&mut self) {
        let Some(nid) = self.poly_normals_target("Auto Smooth") else { return };
        if self.poly_cd(nid).map(|cd| cd.auto_smooth_deg.is_none()).unwrap_or(true) {
            return;
        }
        self.transaction("Auto Smooth (default)", |s| {
            s.poly_write_auto_smooth_angle(nid, None)
        });
        self.toast(&format!(
            "Auto Smooth reset ({:.0}\u{b0})",
            core_math::poly_mesh::AUTO_SMOOTH_ANGLE_DEG
        ));
    }

    /// Mark every edge that an OPERATION created (`op_created` — the Smart Select provenance tags)
    /// as sharp. Cheap because those tags are already authored and persisted by the modelling ops;
    /// this is just a second consumer of them.
    pub(crate) fn poly_mark_sharp_from_smart_select(&mut self) {
        let what = "Mark Sharp from Smart Select";
        let Some(nid) = self.poly_normals_target(what) else { return };
        let Some(m) = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::PolygonalBody { mesh, .. } => Some(mesh.clone()),
            _ => None,
        }) else {
            return;
        };
        let tagged: Vec<(HalfEdgeId, HalfEdgeId)> = match self.poly_cd(nid) {
            Some(cd) => (0..m.half_edges.len() as u32)
                .map(HalfEdgeId)
                .filter(|&h| m.is_edge_live(h) && cd.op_created.get(h))
                .map(|h| (h, m.he(h).twin))
                .collect(),
            None => Vec::new(),
        };
        if tagged.is_empty() {
            self.toast(&format!("{what}: no operation-created edges on this mesh"));
            return;
        }
        let n = tagged.len();
        self.transaction(what, |s| {
            {
                let cd = s.poly_cd_mut(nid);
                for (h, twin) in &tagged {
                    cd.sharp.set(*h, true);
                    if *twin != core_math::half_edge::NO_TWIN {
                        cd.sharp.set(*twin, true);
                    }
                }
            }
            s.overlay_lines_fp = u64::MAX;
            s.poly_force_display_resync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
        });
        self.toast(&format!("{what}: {n} corners"));
    }
}

// =====================================================================================
// APP-PATH gates (memory `feedback_verify_app_path_not_kernel`): the kernel tests in
// `core_math::poly_mesh` prove the MATH; these prove the WIRING — the sidecar actually
// reaches the viewport bake, the display cache key actually invalidates, and Ctrl+Z
// actually restores the marks. A kernel-only pass would not have caught any of those.
// =====================================================================================
#[cfg(test)]
fn face_normals_all(mesh: &HalfEdgeMesh) -> Vec<glam::Vec3> {
    (0..mesh.faces.len())
        .map(|i| mesh.compute_face_normal(core_math::half_edge::FaceId(i as u32)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_math::half_edge::VertexId;
    use core_math::scene_graph::NodePayload;
    use crate::FourthLayerApp;



    /// DIAGNOSTIC (owner 2026-08-06, "EDGE CASE INSET.fl4"): after intrude + clean mesh the
    /// result is "visually perfect but some vertex was not merged properly to segment".
    ///
    /// That symptom is a T-JUNCTION: a vertex sitting ON another edge's interior without being
    /// part of it. It renders perfectly - the surface is closed to the eye - while the topology
    /// is wrong, which is why it survives a visual check and breaks the next bevel or boolean.
    /// Also reports coincident-but-distinct vertices (an unwelded pair at one position).
    ///
    ///   `AZRA_DIAG_FL4="<path>" cargo test -p app diag_fl4_welds -- --nocapture`
    #[test]
    fn diag_fl4_welds() {
        use core_math::half_edge::{HalfEdgeId, VertexId, NO_EDGE};
        let Ok(path) = std::env::var("AZRA_DIAG_FL4") else {
            eprintln!("set AZRA_DIAG_FL4=<path to .fl4>");
            return;
        };
        let pf = crate::project::load_binary(std::path::Path::new(&path)).expect("load_binary");
        let Some(sg) = pf.scene_graph.as_ref() else { return };
        for (nid, node) in sg.all_nodes() {
            let NodePayload::PolygonalBody { name, mesh, .. } = &node.payload else { continue };
            let live: Vec<VertexId> = (0..mesh.vertices.len() as u32)
                .map(VertexId)
                .filter(|&v| mesh.vert(v).half_edge != NO_EDGE)
                .collect();
            eprintln!("== body {nid:?} '{name}' verts={} faces={}", live.len(), mesh.face_count());

            // 1) Coincident but DISTINCT vertices - an unwelded pair.
            let q = |p: glam::Vec3| ((p.x*1e5).round() as i64,(p.y*1e5).round() as i64,(p.z*1e5).round() as i64);
            let mut by_pos: std::collections::HashMap<(i64,i64,i64), Vec<VertexId>> = Default::default();
            for &v in &live { by_pos.entry(q(mesh.vert(v).position)).or_default().push(v); }
            let dups: Vec<_> = by_pos.iter().filter(|(_, vs)| vs.len() > 1).collect();
            eprintln!("   coincident-but-distinct vertex groups = {}", dups.len());
            for (k, vs) in dups.iter().take(6) {
                eprintln!("      {:?} <- {} vertices {:?}", k, vs.len(), vs);
            }

            // 2) T-junctions: a vertex lying strictly INSIDE an edge it does not belong to.
            let mut tj = 0usize;
            let mut shown = 0usize;
            for i in 0..mesh.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if !mesh.is_edge_live(h) { continue; }
                let tw = mesh.he(h).twin;
                if tw != core_math::half_edge::NO_TWIN && tw.0 < i { continue; } // undirected once
                let (a, b) = (mesh.he_source(h), mesh.he(h).vertex);
                let (pa, pb) = (mesh.vert(a).position, mesh.vert(b).position);
                let ab = pb - pa;
                let len2 = ab.length_squared();
                if len2 < 1e-12 { continue; }
                for &v in &live {
                    if v == a || v == b { continue; }
                    let p = mesh.vert(v).position;
                    let t = (p - pa).dot(ab) / len2;
                    if !(0.001..=0.999).contains(&t) { continue; }
                    let d = (p - (pa + ab * t)).length();
                    if d < 1e-4 {
                        tj += 1;
                        if shown < 8 {
                            eprintln!("      T-JUNCTION: v{} {:?} lies on edge v{}-v{} at t={t:.3} (gap {d:.2e})",
                                v.0, p, a.0, b.0);
                            shown += 1;
                        }
                    }
                }
            }
            eprintln!("   T-junction vertices = {tj}");
            eprintln!("   validate={:?} manifold={:?}", mesh.validate().is_ok(), mesh.is_manifold().is_ok());
            // Does Mesh Cleanup actually repair them on the owner's own data?
            let opts = core_math::mesh_boolean::CleanupOptions::default();
            match core_math::mesh_boolean::cleanup_mesh_with(mesh, &opts) {
                None => eprintln!("   AFTER CLEANUP: declined (no change)"),
                Some(c) => {
                    let live2: Vec<VertexId> = (0..c.vertices.len() as u32)
                        .map(VertexId)
                        .filter(|&v| c.vert(v).half_edge != NO_EDGE)
                        .collect();
                    let mut tj2 = 0usize;
                    for i in 0..c.half_edges.len() as u32 {
                        let h = HalfEdgeId(i);
                        if !c.is_edge_live(h) { continue; }
                        let (a, b) = (c.he_source(h), c.he(h).vertex);
                        let (pa, pb) = (c.vert(a).position, c.vert(b).position);
                        let ab = pb - pa;
                        let l2 = ab.length_squared();
                        if l2 < 1e-12 { continue; }
                        for &v in &live2 {
                            if v == a || v == b { continue; }
                            let p = c.vert(v).position;
                            let t = (p - pa).dot(ab) / l2;
                            if !(0.001..=0.999).contains(&t) { continue; }
                            if (p - (pa + ab * t)).length() < 1e-4 { tj2 += 1; }
                        }
                    }
                    eprintln!(
                        "   AFTER CLEANUP: verts={} faces={} T-junctions={tj2} valid={:?}",
                        live2.len(), c.face_count(), c.validate().is_ok()
                    );
                }
            }
        }
    }

    /// DIAGNOSTIC (owner 2026-08-05, "NORMAL BREAK.fl4"): PBR black shading that survives moving
    /// the light. Loads a real `.fl4` and reports, per poly body, exactly what the DISPLAY bake
    /// would emit — because a synthetic bevelled cube emits none of it (poly_mesh.rs gate).
    ///
    ///   `AZRA_DIAG_FL4="<path>" cargo test -p app diag_fl4_normals -- --nocapture`
    #[test]
    fn diag_fl4_normals() {
        let Ok(path) = std::env::var("AZRA_DIAG_FL4") else {
            eprintln!("set AZRA_DIAG_FL4=<path to .fl4>");
            return;
        };
        let pf = crate::project::load_binary(std::path::Path::new(&path)).expect("load_binary");
        let Some(sg) = pf.scene_graph.as_ref() else {
            eprintln!("no scene graph in file");
            return;
        };
        // Lights in the file — an unlit facet with no ambient floor is BLACK by arithmetic, not
        // by any bug, so we must know where the sun actually points before blaming geometry.
        let mut suns: Vec<glam::Vec3> = Vec::new();
        for (_lid, lnode) in sg.all_nodes() {
            if let NodePayload::Light(l) = &lnode.payload {
                let d = glam::Vec3::from_array(l.direction());
                eprintln!(
                    "== light '{}' type={:?} dir={:?} intensity={}",
                    l.name, l.light_type, d, l.intensity
                );
                suns.push(glam::Vec3::from(d).normalize_or_zero());
            }
        }
        for (nid, node) in sg.all_nodes() {
            let NodePayload::PolygonalBody { name, mesh, modifiers, material, .. } = &node.payload
            else {
                continue;
            };
            eprintln!(
                "== body {nid:?} '{name}' verts={} faces={} | mods: {}",
                mesh.vertex_count(),
                mesh.face_count(),
                if modifiers.is_empty() {
                    "none".into()
                } else {
                    modifiers
                        .iter()
                        .map(|m| format!("{:?}(on={})", m.mod_type, m.enabled))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            );
            eprintln!(
                "   albedo={:?} metallic={:.2} roughness={:.2}",
                material.albedo, material.metallic, material.roughness
            );
            // Bake with the KERNEL DEFAULT config: this isolates geometry from the sidecar,
            // which is not carried in the graph blob.
            let cfg = core_math::poly_mesh::NormalConfig::default();
            let (verts, idx, _r) = core_math::poly_mesh::raster_data_ngon_with_normals(mesh, &cfg);
            let (mut zero, mut nan) = (0usize, 0usize);
            for v in &verts {
                let n = glam::Vec3::new(v[3], v[4], v[5]);
                if !n.is_finite() {
                    nan += 1;
                } else if n.length() < 0.5 {
                    zero += 1;
                }
            }
            eprintln!("   emitted verts={} tris={} | ZERO-normal={zero} NON-FINITE={nan}",
                verts.len(), idx.len() / 3);
            // Degenerate faces are the usual source of a zero normal.
            let mut degen = 0usize;
            for f in 0..mesh.faces.len() as u32 {
                let fid = core_math::half_edge::FaceId(f);
                if mesh.face(fid).half_edge == core_math::half_edge::NO_EDGE {
                    continue;
                }
                if mesh.compute_face_normal(fid).length() < 1e-6 {
                    degen += 1;
                }
            }
            eprintln!("   faces with a ZERO area/normal = {degen}");
            eprintln!("   validate={:?} manifold={:?}", mesh.validate().is_ok(), mesh.is_manifold().is_ok());
            // Winding: a closed solid wound OUTWARD has POSITIVE signed volume. Negative means
            // inside-out - every face normal points into the solid, which under a matcap (which
            // ignores lights entirely) reads as a black mass that moving a light cannot change.
            let vol = core_math::mesh_boolean::mesh_signed_volume(mesh);
            eprintln!("   signed volume = {vol:.6}  ({})", if vol < 0.0 { "INSIDE-OUT" } else { "outward" });
            // Per-face agreement with the outward direction from the mesh centre.
            let (lo, hi) = mesh.bounding_box();
            let c = (lo + hi) * 0.5;
            let mut away = 0usize;
            for f in 0..mesh.faces.len() as u32 {
                let fid = core_math::half_edge::FaceId(f);
                if mesh.face(fid).half_edge == core_math::half_edge::NO_EDGE { continue; }
                let vs = mesh.face_vertices(fid);
                if vs.len() < 3 { continue; }
                let ctr: glam::Vec3 = vs.iter().map(|&v| mesh.vert(v).position).sum::<glam::Vec3>() / vs.len() as f32;
                if mesh.compute_face_normal(fid).normalize_or_zero().dot((ctr - c).normalize_or_zero()) < -0.05 { away += 1; }
            }
            eprintln!("   faces pointing INWARD (vs bbox centre) = {away} of {}", mesh.face_count());
            // SMOOTHING DEVIATION. A face is geometrically fine and still shades black when the
            // AVERAGED corner normal is bent far off the face it belongs to - the classic dark
            // band across a narrow bevel strip, where the fan pulls the strip's corners toward
            // its two large neighbours. Report the worst offenders per face.
            let mut worst: Vec<(f32, u32, usize)> = Vec::new();
            for f in 0..mesh.faces.len() as u32 {
                let fid = core_math::half_edge::FaceId(f);
                if mesh.face(fid).half_edge == core_math::half_edge::NO_EDGE { continue; }
                let fnrm = mesh.compute_face_normal(fid).normalize_or_zero();
                if fnrm == glam::Vec3::ZERO { continue; }
                let vs = mesh.face_vertices(fid);
                let mut worst_deg = 0.0f32;
                for &vid in &vs {
                    let cn = core_math::poly_mesh::auto_smooth_corner_normal(
                        mesh, &face_normals_all(mesh), vid, fid, cfg.cos_thresh(), cfg.sharp,
                    );
                    let d = cn.normalize_or_zero().dot(fnrm).clamp(-1.0, 1.0).acos().to_degrees();
                    if d > worst_deg { worst_deg = d; }
                }
                if worst_deg > 45.0 { worst.push((worst_deg, f, vs.len())); }
            }
            // How many faces face AWAY from every light? Those are black with no ambient.
            if !suns.is_empty() {
                let mut unlit = 0usize;
                let mut unlit_area = 0.0f32;
                let mut total_area = 0.0f32;
                for f in 0..mesh.faces.len() as u32 {
                    let fid = core_math::half_edge::FaceId(f);
                    if mesh.face(fid).half_edge == core_math::half_edge::NO_EDGE { continue; }
                    let n = mesh.compute_face_normal(fid).normalize_or_zero();
                    let vs = mesh.face_vertices(fid);
                    let a = if vs.len() >= 3 { 1.0 } else { 0.0 };
                    total_area += a;
                    // Light DIRECTION is the direction the light travels; N.(-L) > 0 = lit.
                    if suns.iter().all(|l| n.dot(-*l) <= 0.0) { unlit += 1; unlit_area += a; }
                }
                eprintln!("   faces facing AWAY from every light = {unlit} of {} ({:.0}%)",
                    mesh.face_count(), 100.0 * unlit_area / total_area.max(1.0));
            }
            worst.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
            eprintln!("   faces whose smoothed corner bends >45 deg off their own face = {}", worst.len());
            for (deg, f, nv) in worst.iter().take(8) {
                eprintln!("      face {f} ({nv}-gon): worst corner deviates {deg:.1} deg");
            }
        }
    }

    /// Headless app holding ONE poly body: a bent 3x3 quad grid whose every dihedral is inside
    /// the 40 degree default, so the default bake smooths and any split is caused by an authored
    /// mark rather than by the angle.
    fn app_with_grid() -> (FourthLayerApp, NodeId) {
        let idx = |i: usize, j: usize| i * 4 + j;
        let mut p = Vec::with_capacity(16);
        for i in 0..4usize {
            for j in 0..4usize {
                let x = i as f32 - 1.5;
                p.push(glam::Vec3::new(x, 0.15 * x * x, j as f32 - 1.5));
            }
        }
        let mut f: Vec<Vec<usize>> = Vec::with_capacity(9);
        for i in 0..3usize {
            for j in 0..3usize {
                f.push(vec![idx(i, j), idx(i, j + 1), idx(i + 1, j + 1), idx(i + 1, j)]);
            }
        }
        let mesh = HalfEdgeMesh::from_polygons(&p, &f);
        let mut app = FourthLayerApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "Grid".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                // Hand-built fixture mesh: authored, not derived from a PrimitiveSpec.
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        (app, nid)
    }

    fn body_mesh(app: &FourthLayerApp, nid: NodeId) -> Arc<HalfEdgeMesh> {
        match &app.scene.scene_graph.get(nid).unwrap().payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh.clone(),
            _ => unreachable!("probe body must be polygonal"),
        }
    }

    /// Grid vertex id by position (positions are unique in the probe mesh).
    fn gvert(m: &HalfEdgeMesh, i: usize, j: usize) -> VertexId {
        let x = i as f32 - 1.5;
        let want = glam::Vec3::new(x, 0.15 * x * x, j as f32 - 1.5);
        (0..m.vertices.len() as u32)
            .map(VertexId)
            .find(|&v| m.is_vert_live(v) && (m.vert(v).position - want).length() < 1e-5)
            .expect("grid vertex")
    }

    /// Select the straight edge line at x-index `i` (running along Z) into the poly selection.
    fn select_edge_line(app: &mut FourthLayerApp, nid: NodeId, i: usize) {
        let m = body_mesh(app, nid);
        app.poly_sel.selected_edges.clear();
        for j in 0..3 {
            let (a, b) = (gvert(&m, i, j), gvert(&m, i, j + 1));
            let h = (0..m.half_edges.len() as u32)
                .map(HalfEdgeId)
                .find(|&h| {
                    m.is_edge_live(h) && {
                        let (s, d) = (m.he_source(h), m.he(h).vertex);
                        (s == a && d == b) || (s == b && d == a)
                    }
                })
                .expect("edge of the line");
            app.poly_sel.selected_edges.insert(h.0);
        }
        app.poly_work_mode = crate::PolyWorkMode::Edge;
    }

    /// The corner normals the VIEWPORT bake emits at a position: goes through the real
    /// `poly_normal_state` -> `raster_data_ngon_with_normals` path, not the kernel default.
    fn baked_normals_at(app: &FourthLayerApp, nid: NodeId, pos: glam::Vec3) -> Vec<glam::Vec3> {
        let m = body_mesh(app, nid);
        let st = app.poly_normal_state(nid, &m);
        let (verts, _, _) = core_math::poly_mesh::raster_data_ngon_with_normals(&m, &st.cfg());
        verts
            .iter()
            .filter(|v| (glam::Vec3::new(v[0], v[1], v[2]) - pos).length() < 1e-5)
            .map(|v| glam::Vec3::new(v[3], v[4], v[5]).normalize_or_zero())
            .collect()
    }

    fn distinct(ns: &[glam::Vec3]) -> usize {
        let mut out: Vec<glam::Vec3> = Vec::new();
        for n in ns {
            if !out.iter().any(|d| d.dot(*n) > 0.9999) {
                out.push(*n);
            }
        }
        out.len()
    }

    /// APP GATE - Mark Sharp through the real action: the bake splits, the display cache key
    /// moves (so the stale GPU mesh cannot be reused), and Ctrl+Z restores BOTH.
    #[test]
    fn mark_sharp_action_splits_the_bake_and_invalidates_the_cache_key_and_undoes() {
        let (mut app, nid) = app_with_grid();
        let m = body_mesh(&app, nid);
        let probe = m.vert(gvert(&m, 2, 1)).position;
        let key_before = app.poly_display_cache_key(nid, &m, &[]);
        assert_eq!(distinct(&baked_normals_at(&app, nid, probe)), 1, "default bake is smooth");
        assert!(app.poly_normal_key(nid, &m).is_none(), "nothing authored yet");

        select_edge_line(&mut app, nid, 2);
        app.poly_set_sharp(true);

        assert_eq!(
            app.poly_cd(nid).map(|cd| cd.sharp.len()).unwrap_or(0),
            6,
            "3 edges x both half-edges: the writer sets the twin, so the mark is side-agnostic"
        );
        assert_eq!(
            distinct(&baked_normals_at(&app, nid, probe)),
            2,
            "the VIEWPORT bake must split at the marked line"
        );
        let key_after = app.poly_display_cache_key(nid, &m, &[]);
        assert_ne!(
            key_before, key_after,
            "the display cache key MUST move, or the stale GPU mesh is reused and the viewport \
             never updates (the Smart Select stale-key bug class)"
        );

        // ---- Ctrl+Z ----
        app.undo();
        assert_eq!(
            app.poly_cd(nid).map(|cd| cd.sharp.len()).unwrap_or(0),
            0,
            "undo clears the marks"
        );
        assert_eq!(distinct(&baked_normals_at(&app, nid, probe)), 1, "undo re-merges the bake");
        assert_eq!(
            app.poly_display_cache_key(nid, &m, &[]),
            key_before,
            "undo restores the original display cache key"
        );
    }

    /// APP GATE - Shade Flat / Shade Smooth through the real actions, plus undo.
    #[test]
    fn shade_flat_and_smooth_actions_drive_the_bake() {
        let (mut app, nid) = app_with_grid();
        let m = body_mesh(&app, nid);
        let probe = m.vert(gvert(&m, 2, 1)).position;

        app.poly_shade_flat();
        assert_eq!(app.poly_cd(nid).unwrap().auto_smooth_angle(), 0.0);
        // 4 incident faces, but the probe grid bends only along X, so the two faces of a column
        // are parallel: per-face normals give exactly 2 distinct values here (vs 1 when smoothed).
        // The "every corner carries ITS face normal" invariant is pinned corner-by-corner by
        // `core_math::poly_mesh::tests::shade_flat_is_per_face_and_shade_smooth_averages_the_full_fan`.
        assert_eq!(
            distinct(&baked_normals_at(&app, nid, probe)),
            2,
            "Shade Flat: each side keeps its own face normal instead of merging into one"
        );

        app.poly_shade_smooth();
        assert_eq!(app.poly_cd(nid).unwrap().auto_smooth_angle(), 180.0);
        assert_eq!(distinct(&baked_normals_at(&app, nid, probe)), 1, "Shade Smooth: one fan");

        app.undo(); // back to flat
        assert_eq!(app.poly_cd(nid).unwrap().auto_smooth_angle(), 0.0, "undo reverts Shade Smooth");
        app.undo(); // back to unauthored
        assert!(
            app.poly_cd(nid).map(|cd| cd.auto_smooth_deg.is_none()).unwrap_or(true),
            "undo reverts Shade Flat to the unauthored default"
        );
        assert_eq!(distinct(&baked_normals_at(&app, nid, probe)), 1, "default is smooth here");
    }

    /// APP GATE - an untouched body is bit-for-bit what it always was: no sidecar, no normal key,
    /// and the bake equals the plain `raster_data_ngon_with_materials` default.
    #[test]
    fn untouched_body_bakes_exactly_as_before() {
        let (app, nid) = app_with_grid();
        let m = body_mesh(&app, nid);
        assert!(app.poly_cd(nid).is_none(), "no sidecar is created just by existing");
        assert!(app.poly_normal_key(nid, &m).is_none(), "no key contribution");
        let st = app.poly_normal_state(nid, &m);
        assert_eq!(st.angle_deg, core_math::poly_mesh::AUTO_SMOOTH_ANGLE_DEG);
        assert!(st.sharp.is_none());
        let (va, ia, ra) = core_math::poly_mesh::raster_data_ngon_with_normals(&m, &st.cfg());
        let (vb, ib, rb) = core_math::poly_mesh::raster_data_ngon_with_materials(&m);
        let bits = |v: &[[f32; 8]]| -> Vec<u32> {
            v.iter().flat_map(|r| r.iter().map(|f| f.to_bits())).collect()
        };
        assert_eq!(bits(&va), bits(&vb), "untouched body must bake byte-identically");
        assert_eq!(ia, ib);
        assert_eq!(ra, rb);
    }
}
