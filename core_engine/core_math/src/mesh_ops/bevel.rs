//! Bevel as the CANONICAL `MeshOp` reference template (plan §4.7, §7 Phase 1).
//!
//! ONE bevel code path: this `MeshOp` impl DELEGATES to `crate::bevel_op::bevel_edges_offset` (the
//! pure offset-rail + `WeldMap` algorithm). Bevel-as-tool (the app drag) and Bevel-as-modifier both
//! route through this one implementation — no duplicate. The geometry lives in `bevel_op.rs`; this
//! file is the typed-operator wrapper that supplies provenance + inverse for the undo/attribute
//! layers.
//!
//! Bevel is a WHOLE-MESH op (it can touch ~all elements), so per plan §4.6 its honest inverse is a
//! per-BODY snapshot (`OpDelta::Snapshot`), not an O(touched) element delta — captured here.

use std::collections::HashSet;

use serde::Serialize;

use crate::bevel_op::{bevel_edges_offset, BevelParams};
use crate::mesh_ops::{EditResult, Handle, MeshOp, OpDelta, OpError, TopoDelta};
use crate::poly_mesh::PolyMesh;

/// Serializable bevel parameters (retained for parametric re-edit, §4.6). Mirrors
/// `bevel_op::BevelParams` but is `Serialize` for the op-record history. `width` is the only field
/// v1 consumes; `segments`/`shape` are carried for v2.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct BevelOpParams {
    pub width: f32,
    pub segments: u32,
    pub shape: f32,
    pub clamp: bool,
}

impl Default for BevelOpParams {
    fn default() -> Self { Self { width: 0.1, segments: 1, shape: 0.5, clamp: true } }
}

impl From<BevelOpParams> for BevelParams {
    fn from(p: BevelOpParams) -> Self {
        BevelParams { width: p.width, segments: p.segments, shape: p.shape, clamp: p.clamp }
    }
}

/// The bevel operator. Stateless — all inputs come through `apply`.
pub struct BevelOp;

impl MeshOp for BevelOp {
    type Params = BevelOpParams;

    /// Bevel does not yet emit a parent-map (Phase 4 / Gate 2c), so it declares the layer drop
    /// loudly via `PROPAGATES = false`. v1 ships pure geometry; attribute flow comes later.
    const PROPAGATES: bool = false;

    fn apply(&self, m: &PolyMesh, sel: &[Handle], p: &Self::Params)
        -> Result<EditResult, OpError>
    {
        if p.width < 0.0 {
            return Err(OpError::InvalidParams { reason: "bevel width must be non-negative".into() });
        }
        if sel.is_empty() {
            return Err(OpError::EmptySelection);
        }

        // Capture the before-image for the inverse BEFORE mutating (per-BODY snapshot tier, §4.6).
        let inverse = OpDelta::Snapshot(m.topo.clone());

        // Selection handles → the half-edge id set the pure op consumes. Raw ids stay
        // kernel-internal (handle.id is the live field pre-GenTags, §4.4).
        let selected: HashSet<u32> = sel.iter().map(|h| h.id).collect();

        let new_topo = bevel_edges_offset(&m.topo, &selected, &(*p).into());

        // Guard the result — a topology violation is a real bug, never silenced (No-Bandaids).
        new_topo.validate().map_err(|reason| OpError::InvalidResult { reason })?;

        let mut out = m.clone();
        out.set_topo(new_topo); // bumps topo_gen

        Ok(EditResult { mesh: out, parents: TopoDelta::new(), inverse })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use crate::half_edge::{HalfEdgeMesh, HalfEdgeId, NO_TWIN, NO_FACE};

    fn interior_edge(m: &HalfEdgeMesh) -> u32 {
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin != NO_TWIN && he.face != NO_FACE && m.he(he.twin).face != NO_FACE {
                return i;
            }
        }
        panic!("no interior edge");
    }

    /// BevelOp (the MeshOp) drives the canonical bevel_op path end-to-end: real geometry, topo_gen
    /// bumped, snapshot inverse captured, result validated.
    #[test]
    fn bevel_op_applies_real_geometry() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let e = interior_edge(&pm.topo);
        let res = BevelOp
            .apply(&pm, &[Handle::new(e)], &BevelOpParams { width: 0.2, ..Default::default() })
            .expect("bevel op must succeed");
        assert_eq!(res.mesh.topo.vertex_count(), 10, "BevelOp must produce real bevel (8→10), not clone");
        assert_eq!(res.mesh.topo_gen, 1, "BevelOp must bump topo_gen");
        res.mesh.topo.validate().expect("BevelOp result must be manifold");
        match res.inverse {
            OpDelta::Snapshot(ref m) => assert_eq!(m.vertex_count(), 8, "inverse snapshots the pre-op mesh"),
            OpDelta::Element(_) => panic!("bevel is a whole-mesh op — its honest inverse is a Snapshot, not an element delta"),
        }
        assert!(!BevelOp::PROPAGATES, "bevel declares it drops layers until its parent-map ships");
    }

    #[test]
    fn bevel_op_rejects_empty_selection() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let err = BevelOp.apply(&pm, &[], &BevelOpParams::default()).unwrap_err();
        assert_eq!(err, OpError::EmptySelection);
    }

    #[test]
    fn bevel_op_rejects_negative_width() {
        let pm = PolyMesh::new(HalfEdgeMesh::new_box(Vec3::ONE));
        let e = interior_edge(&pm.topo);
        let err = BevelOp
            .apply(&pm, &[Handle::new(e)], &BevelOpParams { width: -1.0, ..Default::default() })
            .unwrap_err();
        assert!(matches!(err, OpError::InvalidParams { .. }));
    }
}
