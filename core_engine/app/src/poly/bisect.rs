//! Blender Mesh → Bisect — cut poly mesh by a plane (workplane or symmetry plane).

use std::sync::Arc;
use glam::Vec3;
use core_math::scene_graph::NodePayload;
use crate::{AzraApp, DirtyFlags};

/// Which side of the plane to discard after the cut (Blender clear_inner / clear_outer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BisectClear {
    /// Leave both sides (cut only).
    None,
    /// Delete geometry on the **negative** side of the plane normal.
    Negative,
    /// Delete geometry on the **positive** side.
    Positive,
}

impl AzraApp {
    /// Resolve the bisect plane: symmetry (if on) → workplane → body center + view forward.
    pub(crate) fn poly_bisect_plane(&self) -> Option<(Vec3, Vec3)> {
        if self.viewport.symmetry.enabled {
            let s = &self.viewport.symmetry;
            let n = s.normal.normalize_or_zero();
            if n.length_squared() > 0.25 {
                return Some((s.origin, n));
            }
        }
        let wp = &self.viewport.wp_resolved;
        let n = wp.normal.normalize_or_zero();
        if n.length_squared() > 0.25 {
            return Some((wp.origin, n));
        }
        // Fallback: body AABB center, cut facing the camera.
        let nid = self.sel.usel.active_node()?;
        let node = self.scene.scene_graph.get(nid)?;
        let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
            return None;
        };
        let (mn, mx) = mesh.bounding_box();
        let origin = (mn + mx) * 0.5;
        let cam_fwd = (self.viewport.camera.orbit * Vec3::new(0.0, 0.0, -1.0)).normalize_or_zero();
        if cam_fwd.length_squared() < 0.25 {
            return None;
        }
        Some((origin, cam_fwd))
    }

    /// One-shot Bisect — cut only (keep both halves). Plane: symmetry → workplane → view.
    pub(crate) fn poly_bisect(&mut self) {
        self.poly_bisect_with(BisectClear::None);
    }

    /// Bisect + clear the negative half (plane normal points toward kept side).
    pub(crate) fn poly_bisect_clear_inner(&mut self) {
        self.poly_bisect_with(BisectClear::Negative);
    }

    /// Bisect active poly body with the resolved plane and optional half-delete.
    pub(crate) fn poly_bisect_with(&mut self, clear: BisectClear) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => {
                self.toast("Bisect: select a poly mesh");
                return;
            }
        };
        let Some((origin, normal)) = self.poly_bisect_plane() else {
            self.toast("Bisect: no plane (enable symmetry or workplane)");
            return;
        };
        let n = normal.normalize_or_zero();
        if n.length_squared() < 0.25 {
            self.toast("Bisect: invalid plane normal");
            return;
        }

        // Dry-run: refuse without history if the plane misses the mesh (cut-only).
        let dry_ok = {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                return;
            };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                self.toast("Bisect: active object is not a poly mesh");
                return;
            };
            let mut scratch = (**mesh).clone();
            let before_f = scratch.face_count();
            let before_v = scratch.vertex_count();
            let seam = scratch.bisect_plane(origin, n, 1e-5);
            match clear {
                BisectClear::None => {
                    !seam.is_empty()
                        || scratch.face_count() != before_f
                        || scratch.vertex_count() != before_v
                }
                BisectClear::Negative | BisectClear::Positive => true,
            }
        };
        if !dry_ok {
            self.toast("Bisect: plane does not cut the mesh");
            return;
        }

        let label = match clear {
            BisectClear::None => "Bisect",
            BisectClear::Negative => "Bisect (clear −)",
            BisectClear::Positive => "Bisect (clear +)",
        };
        self.transaction(label, |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let m = Arc::make_mut(mesh);
                    let _seam = m.bisect_plane(origin, n, 1e-5);
                    match clear {
                        BisectClear::None => {}
                        BisectClear::Negative => m.delete_half(origin, n),
                        BisectClear::Positive => m.delete_half(origin, -n),
                    }
                    m.recompute_normals();
                }
            }
            s.poly_sel.selected_faces.clear();
            s.poly_sel.selected_edges.clear();
            s.poly_sel.selected_vertices.clear();
            s.poly_invalidate_pick_cache();
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::MESH);
            s.toast(match clear {
                BisectClear::None => "Bisected",
                BisectClear::Negative => "Bisected (cleared − side)",
                BisectClear::Positive => "Bisected (cleared + side)",
            });
        });
    }
}
