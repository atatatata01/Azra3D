//! Parametric-primitive state on a `PolygonalBody` — the Gate 2 half of
//! `docs/planning/poly/PARAMETRIC_PRIMITIVES.md`.
//!
//! # The invariant, and how it is enforced
//!
//! > When a body's `source` is `Primitive(spec)`, its `mesh` is byte-equal to `spec.build()`.
//!
//! [`AzraApp::set_primitive_spec`] is the ONLY function that writes a spec, and it always rebuilds
//! the mesh in the same breath, so the pair cannot be written out of step.
//!
//! The harder direction is the ~80 places that mutate a body's mesh WITHOUT going through here
//! (bevel, boolean, knife, extrude, slide, …). Rather than require every one of them to remember
//! to clear the spec — the definition of a fix that needs tribal knowledge — `MeshSource` carries
//! the `content_signature()` of the mesh it built, and [`AzraApp::poly_live_spec`] refuses to
//! return a spec whose signature no longer matches. A body whose mesh was edited by any tool
//! simply stops being parametric, detected rather than maintained.

use core_math::primitive_spec::{MeshSource, PrimitiveSpec};
use core_math::scene_graph::{NodeId, NodePayload};

impl crate::AzraApp {
    /// The body's spec, but only if its mesh is still the geometry that spec produced.
    ///
    /// This is the ONE question the UI should ask. Matching on `MeshSource::Primitive` directly
    /// would hand back a spec that a bevel or boolean has already invalidated.
    pub(crate) fn poly_live_spec(&self, nid: NodeId) -> Option<PrimitiveSpec> {
        match &self.scene.scene_graph.get(nid)?.payload {
            NodePayload::PolygonalBody { mesh, source, .. } => source.live_spec(mesh),
            _ => None,
        }
    }

    /// True when this body claims to be parametric but its mesh has drifted — i.e. the user has
    /// edited the geometry with another tool. Used to explain the missing params in the panel
    /// instead of silently showing nothing.
    pub(crate) fn poly_spec_went_stale(&self, nid: NodeId) -> bool {
        match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, source, .. }) => {
                source.claims_primitive() && source.live_spec(mesh).is_none()
            }
            _ => false,
        }
    }

    /// Write a new spec and rebuild the body's mesh from it. THE spec mutator.
    ///
    /// Marks `RENDER` only — this is a parameter edit, not a structural change. Undo is the
    /// caller's job: live drags mutate each frame and commit ONE `transaction_snapshot_only` on
    /// release, matching the F9 panel and Pipe modifier pattern.
    pub(crate) fn set_primitive_spec(&mut self, nid: NodeId, spec: PrimitiveSpec) -> bool {
        let (new_mesh, new_source) = MeshSource::build(spec);
        let Some(node) = self.scene.scene_graph.get_mut(nid) else {
            return false;
        };
        let NodePayload::PolygonalBody { mesh, source, .. } = &mut node.payload else {
            return false;
        };
        *mesh = std::sync::Arc::new(new_mesh);
        *source = new_source;
        self.poly_request_display_sync(nid);
        self.mark_dirty(crate::DirtyFlags::RENDER);
        true
    }

    /// Convert a parametric body to authored geometry — "Apply" / "Make Editable".
    ///
    /// The mesh is kept exactly as it is; only the claim that it is derived goes away. Returns
    /// false when the body was not parametric, so callers can skip an empty undo entry.
    pub(crate) fn poly_make_editable(&mut self, nid: NodeId) -> bool {
        let Some(node) = self.scene.scene_graph.get_mut(nid) else {
            return false;
        };
        let NodePayload::PolygonalBody { source, .. } = &mut node.payload else {
            return false;
        };
        if !source.claims_primitive() {
            return false;
        }
        *source = MeshSource::Authored;
        self.mark_dirty(crate::DirtyFlags::RENDER);
        true
    }

    /// Auto-convert every parametric body in the current selection before component editing.
    ///
    /// Called from the poly work-mode choke points. Without this, a user could Tab in, move a
    /// vertex, then nudge a slider and watch their edit vanish when the mesh got rebuilt from the
    /// spec. The two honest options were "convert" or "refuse to enter edit mode"; converting is
    /// what every other DCC does, and it is undoable.
    ///
    /// Returns the number of bodies converted, so the caller can toast once.
    pub(crate) fn poly_auto_apply_before_component_edit(&mut self) -> usize {
        let mut targets: Vec<NodeId> = Vec::new();
        if let Some(nid) = self.sel.usel.active_node() {
            if self.scene.scene_graph.get(nid).is_some_and(|n| {
                matches!(&n.payload, NodePayload::PolygonalBody { source, .. } if source.claims_primitive())
            }) {
                targets.push(nid);
            }
        }
        let mut n = 0;
        for nid in targets {
            if self.poly_make_editable(nid) {
                n += 1;
            }
        }
        n
    }

    /// Collect every parametric body for saving. Pairs with [`Self::poly_restore_param_records`].
    pub(crate) fn poly_collect_param_records(&self) -> Vec<crate::project::PolyParamRecord> {
        let mut out = Vec::new();
        for (nid, node) in self.scene.scene_graph.all_nodes() {
            if let NodePayload::PolygonalBody { source, .. } = &node.payload {
                if let MeshSource::Primitive { spec, built_sig } = source {
                    out.push(crate::project::PolyParamRecord {
                        node_id: nid.0,
                        spec: *spec,
                        built_sig: *built_sig,
                    });
                }
            }
        }
        out.sort_by_key(|r| r.node_id); // deterministic on-disk order
        out
    }

    /// Reattach saved specs after load.
    ///
    /// A record whose node is missing or is no longer a poly body is DROPPED, not forced — a
    /// stale id must never resurrect a spec onto unrelated geometry. And because `built_sig` is
    /// saved alongside, a body whose mesh was edited before saving still reads back as authored.
    pub(crate) fn poly_restore_param_records(&mut self, records: &[crate::project::PolyParamRecord]) {
        for r in records {
            let nid = NodeId(r.node_id);
            let Some(node) = self.scene.scene_graph.get_mut(nid) else {
                continue;
            };
            if let NodePayload::PolygonalBody { source, .. } = &mut node.payload {
                *source = MeshSource::Primitive {
                    spec: r.spec,
                    built_sig: r.built_sig,
                };
            }
        }
    }
}
