//! AZRA_SELDIAG — self-driving proof that the SELECTION HIGHLIGHT survives an orbit.
//!
//! Owner 2026-08-07, reported twice:
//!   1. "a selected EDGE on the top of a box shows no highlight, the other three edges of the
//!      same face are highlighted"
//!   2. "selected FACES inside a cut notch show their green from one angle and lose it from
//!      another"
//!
//! Reading the code proves nothing about what the user sees (memory
//! `feedback_verify_app_path_not_kernel`), so this harness drives the REAL app: real poly
//! creation, real `poly_sel` state, real overlay build, real GPU passes, and screenshots the
//! viewport from a full yaw sweep. The PIXELS are the verdict.
//!
//! Run (from `core_engine/`):
//!   AZRA_SELDIAG=<out_dir> cargo run -p app
//!
//! Writes `face_yaw000.png` … `face_yaw315.png` then `edge_yaw000.png` … `edge_yaw315.png`
//! and exits. Every shot in a set must show the highlight; a set where one frame is missing
//! green is the bug.
//!
//! Configuration is deliberately the one that FAILED: Visible-Only ON and "Show backfaces"
//! OFF, i.e. both X-ray escapes closed, so the overlay's own occlusion logic is what is under
//! test.

use crate::{AzraApp, DirtyFlags, PolyWorkMode};
use core_math::half_edge::{FaceId, HalfEdgeId, NO_FACE, NO_TWIN};
use core_math::scene_graph::NodePayload;
use glam::{Quat, Vec3};
use std::sync::atomic::{AtomicU32, Ordering};

/// Selected EDGE segments actually painted this frame.
///
/// The screenshot path copies the GPU viewport TEXTURE, so egui-painted overlays (the edge
/// and vertex highlights) never appear in a shot — only GPU passes like the face fill do.
/// This counter is the pixel-equivalent verdict for the egui path: it is incremented at the
/// exact `painter.line_segment` call that draws a selected edge, so it counts what the user
/// sees, not what the selection set contains.
static EDGES_PAINTED: AtomicU32 = AtomicU32::new(0);
/// Resolved once — the counter must cost nothing when the harness is off.
static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

fn armed() -> bool {
    *ARMED.get_or_init(|| std::env::var_os("AZRA_SELDIAG").is_some())
}

/// Called from `draw_poly.rs` where a selected edge is actually stroked.
pub(crate) fn note_edge_painted() {
    if armed() {
        EDGES_PAINTED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Yaw steps of the orbit sweep (degrees). 8 shots = a full turn at 45-degree steps; the
/// reported dropouts were angle-specific, so a partial arc would not have caught them.
const YAWS: [f32; 8] = [0.0, 45.0, 90.0, 135.0, 180.0, 225.0, 270.0, 315.0];
/// Look down ~35 degrees — the standard 3/4 modelling view, and the one in which a box's four
/// top edges all have their averaged-normal diagonals near the facing threshold.
const PITCH_DEG: f32 = -35.0;

/// Frame numbers: setup, then one shot every `SHOT_STRIDE` frames.
const FACE_SETUP: u32 = 30;
/// Selection is applied AFTER the display sync: `poly_request_display_sync` reconciles
/// `poly_sel` against the rebuilt display mesh, so a set made in the same frame as the
/// topology edit is wiped (observed: `faces=0` in run 1's log).
const FACE_SELECT: u32 = 60;
const FACE_FIRST_SHOT: u32 = 80;
const SHOT_STRIDE: u32 = 10;
const EDGE_SETUP: u32 = FACE_FIRST_SHOT + SHOT_STRIDE * 8 + 20;
const EDGE_FIRST_SHOT: u32 = EDGE_SETUP + 50;
const DONE: u32 = EDGE_FIRST_SHOT + SHOT_STRIDE * 8 + 30;

impl AzraApp {
    /// One tick of the selection-highlight harness. Called from `update()` only when
    /// `AZRA_SELDIAG` is set (a single env read otherwise).
    pub(crate) fn seldiag_tick(&mut self, ctx: &eframe::egui::Context) {
        let out_dir = match std::env::var("AZRA_SELDIAG") {
            Ok(d) if !d.is_empty() => std::path::PathBuf::from(d),
            _ => return,
        };
        // Read+clear at the TOP of the frame: what this returns is the count from the PREVIOUS
        // frame's draw, which makes the number per-frame deterministic (4 selected edges => 4)
        // instead of an accumulation over the shot stride.
        let painted_prev = EDGES_PAINTED.swap(0, Ordering::Relaxed);
        self.visdiff_frame += 1;
        let f = self.visdiff_frame;
        // The capture path lives inside the dirty-gated render (see visdiff.rs) — without a
        // forced dirty every harness frame the shot requests never run.
        self.mark_dirty(DirtyFlags::RENDER);
        ctx.request_repaint();

        match f {
            FACE_SETUP => {
                std::fs::create_dir_all(&out_dir).ok();
                self.seldiag_close_xray_escapes();
                self.create_poly_mesh("box");
                if !self.seldiag_build_notch() {
                    eprintln!("[SELDIAG] FAIL: could not build the notch fixture");
                    std::process::exit(2);
                }
            }
            FACE_SELECT => {
                self.seldiag_close_xray_escapes();
                if !self.seldiag_select_notch_faces() {
                    eprintln!("[SELDIAG] FAIL: could not select the notch faces");
                    std::process::exit(2);
                }
            }
            EDGE_SETUP => {
                self.seldiag_close_xray_escapes();
                if !self.seldiag_select_top_edges() {
                    eprintln!("[SELDIAG] FAIL: could not select the top edges");
                    std::process::exit(2);
                }
            }
            DONE => {
                eprintln!("[SELDIAG] done -> {}", out_dir.display());
                std::process::exit(0);
            }
            _ => {}
        }

        if std::env::var_os("AZRA_SELDIAG_TRACE").is_some()
            && f > EDGE_SETUP
            && self.poly_work_mode == PolyWorkMode::Edge
        {
            eprintln!("[SELDIAG-TRACE] f={f} painted_prev={painted_prev} sel={}",
                      self.poly_sel.selected_edges.len());
        }
        // Shot scheduling: aim the camera one frame BEFORE the capture so the overlay rebuild,
        // the pick buffer and the G-buffer depth all correspond to the pose being photographed.
        for (set, first) in [("face", FACE_FIRST_SHOT), ("edge", EDGE_FIRST_SHOT)] {
            for (i, yaw) in YAWS.iter().enumerate() {
                let shot_f = first + SHOT_STRIDE * i as u32;
                if f == shot_f - 2 {
                    self.seldiag_aim(*yaw);
                } else if f == shot_f {
                    let name = format!("{set}_yaw{:03}.png", *yaw as i32);
                    if self.pending_screenshot.is_some() {
                        eprintln!("[SELDIAG] WARN prior shot pending — dropped {name}");
                        continue;
                    }
                    eprintln!(
                        "[SELDIAG] frame {f} shot {name} (mode={:?}, faces={}, edges={}, \
                         overlay_face_verts={}, EDGES_PAINTED={})",
                        self.poly_work_mode,
                        self.poly_sel.selected_faces.len(),
                        self.poly_sel.selected_edges.len(),
                        self.overlay_faces.len(),
                        painted_prev,
                    );
                    self.visdiff_shot = Some(out_dir.join(name));
                }
            }
        }
    }

    /// Both X-ray escapes OFF — the configuration in which the highlight vanished. With either
    /// one on, the overlay draws unconditionally and the harness would prove nothing.
    fn seldiag_close_xray_escapes(&mut self) {
        // AZRA_SELDIAG_XRAY=1 opens the X-ray escape — used to separate "the overlay never
        // ran" from "the depth gate rejected it".
        let xray = std::env::var("AZRA_SELDIAG_XRAY").as_deref() == Ok("1");
        self.sel.visible_only = !xray;
        self.viewport.poly_sel_show_backface = xray;
        self.viewport.show_overlays = true;
        self.viewport.show_poly_overlays = true;
    }

    /// Point the camera at the body from `yaw` degrees, looking down `PITCH_DEG`.
    fn seldiag_aim(&mut self, yaw_deg: f32) {
        self.viewport.camera.target = Vec3::ZERO;
        self.viewport.camera.distance = 4.5;
        self.viewport.camera.orbit = Quat::from_rotation_y(yaw_deg.to_radians())
            * Quat::from_rotation_x(PITCH_DEG.to_radians());
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Cut a real recess into the top face (inset + negative depth = the owner's notch) and
    /// select its floor plus all four inward-facing walls. Those walls are the faces whose
    /// normals point INWARD — precisely the ones a facing test throws away.
    fn seldiag_build_notch(&mut self) -> bool {
        let Some(nid) = self.sel.usel.active_node() else {
            return false;
        };
        {
            let Some(node) = self.scene.scene_graph.get_mut(nid) else {
                return false;
            };
            let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload else {
                return false;
            };
            let m = std::sync::Arc::make_mut(mesh);
            let Some(top) = top_face(m) else { return false };
            // thickness 0.3, depth -0.4 → a recess, not a boss.
            let floor = m.inset_face_depth(top, 0.3, -0.4);
            if floor == NO_FACE || floor.0 as usize >= m.faces.len() {
                return false;
            }
        }
        self.poly_request_display_sync(nid);
        self.mark_dirty(DirtyFlags::SCENE);
        eprintln!("[SELDIAG] notch cut into the top face");
        true
    }

    /// Select the recess floor + its four INWARD-facing walls. Those walls are exactly the
    /// faces a `dot(normal, view)` cull throws away while they are plainly visible.
    fn seldiag_select_notch_faces(&mut self) -> bool {
        let Some(nid) = self.sel.usel.active_node() else {
            return false;
        };
        let mut picked: Vec<u32> = Vec::new();
        {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                return false;
            };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                return false;
            };
            // The recess floor is the LOWEST face that is not the box bottom: pick the face
            // whose centroid is highest among faces below the rim but above the base.
            let Some(floor) = notch_floor(mesh) else { return false };
            picked.push(floor.0);
            for h in mesh.face_half_edges(floor) {
                let tw = mesh.he(h).twin;
                if tw == NO_TWIN {
                    continue;
                }
                let wf = mesh.he(tw).face;
                if wf != NO_FACE && !picked.contains(&wf.0) {
                    picked.push(wf.0);
                }
            }
        }
        self.poly_work_mode = PolyWorkMode::Face;
        self.poly_sel.selected_faces.clear();
        self.poly_sel.selected_edges.clear();
        self.poly_sel.selected_vertices.clear();
        for p in &picked {
            self.poly_sel.selected_faces.insert(*p);
        }
        self.mark_dirty(DirtyFlags::RENDER);
        eprintln!("[SELDIAG] {} notch faces selected (floor + walls)", picked.len());
        true
    }

    /// Edge mode with the box's four TOP RIM edges selected — the owner's case 1.
    ///
    /// The rim is the outer boundary of the original top face: four edges at max Y, each shared
    /// between the top surface and a side wall. From a 3/4 view looking down, ALL FOUR are
    /// plainly visible and symmetric, so any frame painting fewer than four is the bug — there
    /// is no camera angle at which one of them is legitimately occluded.
    ///
    /// (Earlier revision of this harness selected `top_face`'s loop instead. After the inset,
    /// `top_face` is one NOTCH WALL, whose far-side edges are genuinely hidden — the harness was
    /// measuring correct occlusion and reporting it as a bug. Fixture error, not a code error.)
    fn seldiag_select_top_edges(&mut self) -> bool {
        let Some(nid) = self.sel.usel.active_node() else {
            return false;
        };
        let mut edges: Vec<u32> = Vec::new();
        {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                return false;
            };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                return false;
            };
            let max_y = mesh
                .vertices
                .iter()
                .map(|v| v.position.y)
                .fold(f32::NEG_INFINITY, f32::max);
            for h in 0..mesh.half_edges.len() as u32 {
                let hid = HalfEdgeId(h);
                if !mesh.is_edge_live(hid) {
                    continue;
                }
                // One half-edge per undirected edge.
                let tw = mesh.he(hid).twin;
                if tw != NO_TWIN && tw.0 < h {
                    continue;
                }
                let a = mesh.vert(mesh.he_source(hid)).position.y;
                let b = mesh.vert(mesh.he(hid).vertex).position.y;
                if (a - max_y).abs() < 1e-4 && (b - max_y).abs() < 1e-4 {
                    edges.push(h);
                }
            }
        }
        if edges.is_empty() {
            return false;
        }
        self.poly_work_mode = PolyWorkMode::Edge;
        self.poly_sel.selected_faces.clear();
        self.poly_sel.selected_vertices.clear();
        self.poly_sel.selected_edges.clear();
        for e in &edges {
            self.poly_sel.selected_edges.insert(*e);
        }
        self.mark_dirty(DirtyFlags::RENDER);
        eprintln!("[SELDIAG] edge mode, {} TOP RIM edges selected", edges.len());
        true
    }
}

/// The recess floor: the face whose centroid Y is highest among faces strictly BELOW the top
/// rim. On the inset box that is the notch bottom.
fn notch_floor(m: &core_math::half_edge::HalfEdgeMesh) -> Option<FaceId> {
    let top_y = face_centroid_y(m, top_face(m)?)?;
    let mut best: Option<(FaceId, f32)> = None;
    for i in 0..m.faces.len() as u32 {
        let fid = FaceId(i);
        if !m.is_face_live(fid) || fid == top_face(m)? {
            continue;
        }
        let Some(y) = face_centroid_y(m, fid) else { continue };
        if y >= top_y - 1e-4 {
            continue;
        }
        if best.map_or(true, |(_, by)| y > by) {
            best = Some((fid, y));
        }
    }
    best.map(|(f, _)| f)
}

fn face_centroid_y(m: &core_math::half_edge::HalfEdgeMesh, f: FaceId) -> Option<f32> {
    let vs = m.face_vertices(f);
    if vs.len() < 3 {
        return None;
    }
    Some(vs.iter().map(|&v| m.vert(v).position.y).sum::<f32>() / vs.len() as f32)
}

/// The live face with the highest centroid — the "top of the box".
fn top_face(m: &core_math::half_edge::HalfEdgeMesh) -> Option<FaceId> {
    let mut best: Option<(FaceId, f32)> = None;
    for i in 0..m.faces.len() as u32 {
        let fid = FaceId(i);
        if !m.is_face_live(fid) {
            continue;
        }
        let vs = m.face_vertices(fid);
        if vs.len() < 3 {
            continue;
        }
        let y = vs.iter().map(|&v| m.vert(v).position.y).sum::<f32>() / vs.len() as f32;
        if best.map_or(true, |(_, by)| y > by) {
            best = Some((fid, y));
        }
    }
    best.map(|(f, _)| f)
}

/// Half-edges around a face (harness-local; the kernel exposes vertices, not the loop).
trait FaceLoop {
    fn face_half_edges(&self, f: FaceId) -> Vec<HalfEdgeId>;
}
impl FaceLoop for core_math::half_edge::HalfEdgeMesh {
    fn face_half_edges(&self, f: FaceId) -> Vec<HalfEdgeId> {
        let start = self.face(f).half_edge;
        let mut out = Vec::new();
        if start == core_math::half_edge::NO_EDGE {
            return out;
        }
        let mut h = start;
        for _ in 0..64 {
            out.push(h);
            h = self.he(h).next;
            if h == start || h == core_math::half_edge::NO_EDGE {
                break;
            }
        }
        out
    }
}
