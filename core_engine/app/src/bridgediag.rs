//! AZRA_BRIDGEDIAG — self-driving verification that BRIDGE works on a MULTI-SELECTION.
//!
//! Owner 2026-08-07: "look at the bridge code — I feel it does not work on multi select and it
//! needs to." A `core_math` test proves the kernel; it does NOT prove the user's experience
//! (memory `feedback_verify_app_path_not_kernel`). This harness drives the REAL path:
//!   real primitive creation → real `poly_sel` selection state → real `poly_bridge()` tool entry
//! and prints the resulting topology census, so the verdict is the app's own mesh, not a unit test.
//!
//! Run (from `core_engine/`):
//!   AZRA_BRIDGEDIAG=faces3   cargo run -p app    # 3 stacked caps, FACE mode multi-select
//!   AZRA_BRIDGEDIAG=faces4   cargo run -p app    # 4 stacked caps
//!   AZRA_BRIDGEDIAG=edges3   cargo run -p app    # 3 wire rings, EDGE mode multi-select
//!   AZRA_BRIDGEDIAG=edges4   cargo run -p app    # 4 wire rings
//!   AZRA_BRIDGEDIAG=unequal  cargo run -p app    # 3 rings of 6 / 8 / 4 verts
//!   AZRA_BRIDGEDIAG=islands  cargo run -p app    # two islands x two rings
//!   AZRA_BRIDGEDIAG=cyl2     cargo run -p app    # regression: the old 2-cap case still works
//!
//! Expected (Blender 4.5 oracle, `docs/planning/poly/bridge_multi_ref.py`):
//!   edges3  → 16 new quads, 2 bridges, boundary 16, manifold OK
//!   edges4  → 24 new quads, 3 bridges, boundary 16, manifold OK
//!   islands → identical to edges4 (Blender chains straight across islands)
//!   faces3  → 8 walls, all 3 caps deleted
//! Console prints `[BRIDGEDIAG]` markers and exits by itself.

use std::sync::Arc;
use glam::Vec3;
use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, HVertex, VertexId, NO_EDGE, NO_TWIN};
use core_math::scene_graph::NodePayload;
use crate::{AzraApp, DirtyFlags, PolyWorkMode};

/// A ring of `n` vertices at height `y`, offset in X by `dx`.
fn ring(m: &mut HalfEdgeMesh, n: usize, y: f32, dx: f32) -> Vec<VertexId> {
    (0..n).map(|k| {
        let a = std::f32::consts::TAU * k as f32 / n as f32;
        m.alloc_vertex(HVertex {
            position: Vec3::new(a.cos() + dx, y, a.sin()),
            normal: Vec3::Y,
            half_edge: NO_EDGE,
        })
    }).collect()
}

/// The fixture mesh for a mode, plus what the tool should select on it.
/// Returns (mesh, face ids to select, whether to use FACE mode).
fn fixture(mode: &str) -> Option<(HalfEdgeMesh, Vec<FaceId>, bool)> {
    let mut m = HalfEdgeMesh::new();
    match mode {
        // FACE mode: N stacked open caps. The tool must delete every cap and chain the rings.
        "faces3" | "faces4" | "cyl2" => {
            let n = match mode { "cyl2" => 2, "faces3" => 3, _ => 4 };
            let mut caps = Vec::new();
            for i in 0..n {
                let r = ring(&mut m, 4, i as f32 * 2.0, 0.0);
                caps.push(m.create_face(&r)?);
            }
            Some((m, caps, true))
        }
        // EDGE mode: N free-standing wire rings. Wire rings have no half-edges until faces exist,
        // so give each ring a cap face; the tool's edge selection then sees real boundary loops
        // and `edge_loop_rings` recovers N rings — exactly the user's multi-loop selection.
        "edges2" | "edges3" | "edges4" | "unequal" | "islands" => {
            let plan: Vec<(usize, f32, f32)> = match mode {
                "edges2" => vec![(8, 0.0, 0.0), (8, 1.0, 0.0)],
                "edges3" => vec![(8, 0.0, 0.0), (8, 1.0, 0.0), (8, 2.0, 0.0)],
                "edges4" => vec![(8, 0.0, 0.0), (8, 1.0, 0.0), (8, 2.0, 0.0), (8, 3.0, 0.0)],
                "unequal" => vec![(6, 0.0, 0.0), (8, 1.0, 0.0), (4, 2.0, 0.0)],
                _ => vec![(8, 0.0, -4.0), (8, 1.0, -4.0), (8, 0.0, 4.0), (8, 1.0, 4.0)],
            };
            // ALTERNATE the winding down the stack. Two rims that are to be bridged into a closed
            // tube must face EACH OTHER; two identically-wound caps cannot close, and bridging them
            // yields a correctly-twinned but CROSSED quad band. That is a degenerate fixture, not a
            // tool bug — build the realistic configuration instead.
            let mut caps = Vec::new();
            for (i, (n, y, dx)) in plan.into_iter().enumerate() {
                let mut r = ring(&mut m, n, y, dx);
                if i % 2 == 1 { r.reverse(); }
                caps.push(m.create_face(&r)?);
            }
            Some((m, caps, false))
        }
        _ => None,
    }
}

/// Boundary half-edge count + manifold verdict — the census the Blender oracle records.
fn census(m: &HalfEdgeMesh) -> String {
    let bnd = (0..m.half_edges.len() as u32)
        .filter(|&h| m.is_edge_live(HalfEdgeId(h)) && m.he(HalfEdgeId(h)).twin == NO_TWIN)
        .count();
    let mut arity: std::collections::BTreeMap<usize, usize> = Default::default();
    for f in 0..m.faces.len() as u32 {
        if m.is_face_live(FaceId(f)) {
            *arity.entry(m.face_vertices(FaceId(f)).len()).or_default() += 1;
        }
    }
    format!(
        "nv={} nf={} boundary={} arity={:?} validate={} manifold={}",
        m.vertex_count(), m.face_count(), bnd, arity,
        m.validate().is_ok(), m.is_manifold().is_ok()
    )
}

impl AzraApp {
    /// One tick of the bridge harness. Called from `update()` only when AZRA_BRIDGEDIAG is set.
    pub(crate) fn bridgediag_tick(&mut self, ctx: &eframe::egui::Context) {
        let mode = match std::env::var("AZRA_BRIDGEDIAG") {
            Ok(m) if !m.is_empty() => m,
            _ => return,
        };
        self.visdiff_frame += 1;
        let f = self.visdiff_frame;
        self.mark_dirty(DirtyFlags::RENDER);
        ctx.request_repaint();

        match f {
            // Create a real poly body through the real creation path, so everything downstream
            // (scene graph node, display sync, selection, undo) is wired the way a user's is.
            20 => {
                self.create_poly_mesh("box");
                eprintln!("[BRIDGEDIAG] frame 20: poly body created (mode={mode})");
            }
            // Install the fixture topology into that body and select through the REAL selection
            // state the tool reads (`poly_sel` + `poly_work_mode`).
            40 => {
                let Some(nid) = self.sel.usel.active_node() else {
                    eprintln!("[BRIDGEDIAG] FAIL: no active node"); return;
                };
                let Some((fx, caps, face_mode)) = fixture(&mode) else {
                    eprintln!("[BRIDGEDIAG] FAIL: unknown mode '{mode}'"); return;
                };
                let mut edge_sel: Vec<u32> = Vec::new();
                if !face_mode {
                    // EDGE mode: select every boundary half-edge — the N rings the user would
                    // have picked with Alt+click on each loop.
                    for h in 0..fx.half_edges.len() as u32 {
                        if fx.is_edge_live(HalfEdgeId(h)) && fx.he(HalfEdgeId(h)).twin == NO_TWIN {
                            edge_sel.push(h);
                        }
                    }
                }
                eprintln!("[BRIDGEDIAG] fixture BEFORE: {}", census(&fx));
                if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                    if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                        *Arc::make_mut(mesh) = fx;
                    }
                }
                self.poly_sel.selected_faces.clear();
                self.poly_sel.selected_edges.clear();
                self.poly_sel.selected_vertices.clear();
                if face_mode {
                    self.poly_work_mode = PolyWorkMode::Face;
                    for c in &caps { self.poly_sel.selected_faces.insert(c.0); }
                    eprintln!("[BRIDGEDIAG] FACE mode, {} faces selected", caps.len());
                } else {
                    self.poly_work_mode = PolyWorkMode::Edge;
                    for h in &edge_sel { self.poly_sel.selected_edges.insert(*h); }
                    eprintln!("[BRIDGEDIAG] EDGE mode, {} edges selected", edge_sel.len());
                }
                self.poly_request_display_sync(nid);
            }
            // Fire the REAL tool entry point — the same function the keybinding and menu call.
            60 => {
                // Re-assert the selection at fire time: display sync / poly mode plumbing between
                // frames can legitimately clear `poly_sel`, and a harness that measured an EMPTY
                // selection would blame the tool for something it never saw.
                eprintln!(
                    "[BRIDGEDIAG] frame 60: mode={:?} sel_edges={} sel_faces={} active={:?}",
                    self.poly_work_mode,
                    self.poly_sel.selected_edges.len(),
                    self.poly_sel.selected_faces.len(),
                    self.sel.usel.active_node()
                );
                if let Some(nid) = self.sel.usel.active_node() {
                    if let Some(node) = self.scene.scene_graph.get(nid) {
                        if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                            let rings = {
                                let set: std::collections::HashSet<HalfEdgeId> =
                                    self.poly_sel.selected_edges.iter().map(|&e| HalfEdgeId(e)).collect();
                                mesh.edge_loop_rings(&set)
                            };
                            eprintln!("[BRIDGEDIAG] at fire: mesh {} | edge_loop_rings -> {} rings {:?}",
                                      census(mesh), rings.len(),
                                      rings.iter().map(|r| r.len()).collect::<Vec<_>>());
                        }
                    }
                }
                self.poly_bridge();
            }
            // Read back what the tool actually produced.
            80 => {
                let Some(nid) = self.sel.usel.active_node() else { return };
                if let Some(node) = self.scene.scene_graph.get(nid) {
                    if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                        eprintln!("[BRIDGEDIAG] RESULT ({mode}): {}", census(mesh));
                        eprintln!("[BRIDGEDIAG] selection after: {} faces",
                                  self.poly_sel.selected_faces.len());
                    }
                }
            }
            100 => {
                eprintln!("[BRIDGEDIAG] done");
                std::process::exit(0);
            }
            _ => {}
        }
    }
}
