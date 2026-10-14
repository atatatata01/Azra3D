//! Scene graph — single source of truth for scene nodes (bodies, lights, materials, poly, NURBS).
//!
//! # Module layout (pure relocation)
//!
//! GOD_FILES_SPLIT_PLAN: former single-file `scene_graph.rs` →
//! `scene_graph/{mod,node,payload,transform,materials,queries,validate,tests}.rs`.
//! Public paths `core_math::scene_graph::*` are unchanged. **NodePayload is append-only**
//! (bincode tags by variant index) — the VoxelBody/SdfObject tail warning lives with the enum
//! in [`payload`].

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

use glam::Mat4;

mod node;
mod payload;
mod transform;
mod materials;
mod queries;
mod validate;

#[cfg(test)]
mod tests;

pub use node::*;
pub use payload::*;
pub use transform::{BodyTransform, prims_local_to_world};

// ============================================================
// Scene Graph
// ============================================================

/// The scene graph: a HashMap-backed tree of nodes.
///
/// All scene data (primitives, lights, cameras) is owned here.
/// This is the single source of truth for the scene.
#[derive(Debug, Serialize, Deserialize)]
pub struct SceneGraph {
    nodes: HashMap<NodeId, SceneNode>,
    root: NodeId,
    next_id: u64,
    /// AZRA_MASTER_ROADMAP Phase A5: memoize `world_transform` within a read burst.
    /// Cleared on `get_mut` / hierarchy edits so gizmo moves never leave stale matrices.
    /// Raster gather + overlays only `get` → cache stays hot for the whole pass.
    /// `Mutex` (not `RefCell`) — `SceneGraph` is `Send`/`Sync` for bake workers.
    #[serde(skip)]
    world_xform_cache: Mutex<HashMap<NodeId, Mat4>>,
    /// Material-RESOLUTION generation: changes whenever what a drawable resolves to may have
    /// changed. Bumped on every `material_by_id_mut` / `add_material` / `remove_material`
    /// (graph material content) AND by `note_prim_material_write` when a prim's
    /// `material_id` / embedded `material` changes (assignment), so raster's structural draw
    /// list (which caches resolved `RasterMaterial` bytes) can detect RENDER-only material
    /// edits that do not set SCENE/MESH dirty (M0 class: roughness slider invisible;
    /// RASTER-MATERIAL-REASSIGN-STALE: reassigning an existing material invisible).
    #[serde(skip)]
    material_generation: u64,
}

impl Clone for SceneGraph {
    fn clone(&self) -> Self {
        // Cache is ephemeral — do not copy; clone must match deserialized empty-cache state.
        Self {
            nodes: self.nodes.clone(),
            root: self.root,
            next_id: self.next_id,
            world_xform_cache: Mutex::new(HashMap::new()),
            material_generation: self.material_generation,
        }
    }
}

impl SceneGraph {
    /// Create an empty scene with just a root node.
    pub fn new() -> Self {
        let root_id = NodeId(0);
        let root = SceneNode::new(root_id, NodePayload::Root);
        let mut nodes = HashMap::new();
        nodes.insert(root_id, root);
        Self {
            nodes,
            root: root_id,
            next_id: 1,
            world_xform_cache: Mutex::new(HashMap::new()),
            material_generation: 0,
        }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    fn alloc_id(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }
}

impl Default for SceneGraph {
    fn default() -> Self {
        Self::new()
    }
}
