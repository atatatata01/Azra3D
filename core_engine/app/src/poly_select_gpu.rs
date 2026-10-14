// poly_select_gpu.rs — GPU select buffer for poly sub-element selection (Blender model).
//
// PAINT_SELECT_AUDIT.md P1 + PER-ELEMENT upgrade 2026-08-07: Blender's architecture
// (view3d_select.cc + the select engine) rendered for real — verts, edges AND faces each write
// their OWN tagged id into the picking target, depth-tested in one pass (faces plain, edges and
// verts with a small depth bias so they win ties against their own coplanar faces — draw order
// faces→edges→verts). Other visible poly bodies render as id-0 depth occluders. Every visibility
// question is then an O(1) exact pixel lookup on the element's own id:
//   face visible at sp  ⇔ image[sp] == pick_id_face(f)
//   edge visible at sp  ⇔ image[sp] == pick_id_edge(canonical half-edge)
//   vert visible at sp  ⇔ image[sp] == pick_id_vert(v)
// Depth does the occlusion — no incident-face inference, no rays, no bias constants. (The old
// face-only buffer + "a nearby pixel shows an incident face" inference was the root of the
// silhouette false-culls and behind-surface false-passes — the "selection fails often" class.)
//
// Frame rules honored: the render + buffer creation happen in poll_background_tasks (Frame Ordering
// Contract), ONE readback in flight (Frame Budget), map_async result via atomic flag (no discarded
// Result — the flag pattern is the established picking-pass idiom). The image is keyed on
// (camera, mesh content, transform, hidden set); a camera "settle" gate avoids re-rendering + 8MB
// readback every frame during an orbit — and a pointer-press SYNCHRONOUS refresh (Blender renders
// its select buffer inside the pick operator) closes the stale window at exactly the moment a
// click needs the answer. While the image is stale/pending, `poly_screen_test` falls back to the
// legacy CPU-ray path — behavior-correct, just slower for that frame.

use eframe::egui;
use glam::Vec3;
use core_math::half_edge::{FaceId, HalfEdgeId, VertexId, NO_EDGE, NO_TWIN};
use core_math::scene_graph::NodePayload;
use render::camera::CameraUniforms;
use render::picking::{PickingPipeline, PolyPickVertex};

use crate::AzraApp;

// ============================================================
// Element-tagged pick ids — the ONE encoding for the select buffer.
// Top 2 bits = element class, low 30 bits = index + 1. 0 = background/occluder.
// ============================================================
pub(crate) const PICK_TAG_VERT: u32 = 1 << 30;
pub(crate) const PICK_TAG_EDGE: u32 = 2 << 30;
pub(crate) const PICK_TAG_FACE: u32 = 3 << 30;
pub(crate) const PICK_IDX_MASK: u32 = (1 << 30) - 1;

#[inline]
pub(crate) fn pick_id_vert(v: u32) -> u32 { PICK_TAG_VERT | (v + 1) }
/// Edges are stored under their CANONICAL half-edge (min of the twin pair) — same identity
/// the pick cache and the selection sets use.
#[inline]
pub(crate) fn pick_id_edge(canonical_he: u32) -> u32 { PICK_TAG_EDGE | (canonical_he + 1) }
#[inline]
pub(crate) fn pick_id_face(f: u32) -> u32 { PICK_TAG_FACE | (f + 1) }
#[inline]
pub(crate) fn pick_id_to_face(id: u32) -> Option<u32> {
    (id & !PICK_IDX_MASK == PICK_TAG_FACE).then(|| (id & PICK_IDX_MASK) - 1)
}
/// Canonical half-edge id of `he` (min of the twin pair) for edge pick-id lookups.
#[inline]
pub(crate) fn canonical_he(mesh: &core_math::half_edge::HalfEdgeMesh, he: u32) -> u32 {
    let tw = mesh.he(HalfEdgeId(he)).twin;
    if tw != NO_TWIN { he.min(tw.0) } else { he }
}

/// The CPU copy of the poly select buffer (one whole-viewport ID image).
pub(crate) struct PolyPickImage {
    pub rect: egui::Rect, // viewport rect the image was rendered for (query coords are viewport points)
    pub w: u32,
    pub h: u32,
    pub ids: Vec<u32>, // w*h, row-major; 0 = background/occluder, else tagged element id (PICK_TAG_* | idx+1)
    pub cam_key: u64,
    pub mesh_key: u64,
}

impl PolyPickImage {
    #[inline]
    fn id_at_px(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 { return 0; }
        self.ids[y as usize * self.w as usize + x as usize]
    }

    /// True if any pixel in the 3×3 neighborhood of viewport-point `sp` satisfies `pred`.
    /// The neighborhood absorbs rasterization off-by-one at element boundaries (an edge's own
    /// pixels belong to one of its faces; a vert sits at a corner of its fan).
    #[inline]
    pub(crate) fn any_near(&self, sp: egui::Pos2, pred: impl Fn(u32) -> bool) -> bool {
        self.any_near_r(sp, 1, pred)
    }

    /// Like [`Self::any_near`] with a configurable Chebyshev radius in **pixels** (`r=1` → 3×3,
    /// `r=3` → 7×7). Edges use a wider probe: a 1 px ring misses when the cursor sits several
    /// pixels off the silhouette or when faces are edge-on and only a few ID pixels land.
    #[inline]
    pub(crate) fn any_near_r(&self, sp: egui::Pos2, r: i32, pred: impl Fn(u32) -> bool) -> bool {
        let px = (sp.x - self.rect.min.x) as i32;
        let py = (sp.y - self.rect.min.y) as i32;
        let r = r.max(0);
        for dy in -r..=r {
            for dx in -r..=r {
                let id = self.id_at_px(px + dx, py + dy);
                if id != 0 && pred(id) {
                    return true;
                }
            }
        }
        false
    }

    /// Face id of the nearest FACE-tagged pixel within Chebyshev radius `r` of `sp`, scanned
    /// outward ring by ring (exact pixel first).
    ///
    /// Pass 2 of the select render draws edges and verts OVER the face ids, so the exact pixel
    /// under the cursor can carry a rim element's id even when the cursor is well inside a face.
    /// The outward scan steps past that 1 px rim; it is the same "neighbourhood absorbs raster
    /// slop" contract `any_near_r` already uses for verts and edges, applied to the face channel.
    #[inline]
    pub(crate) fn nearest_face_at(&self, sp: egui::Pos2, r: i32) -> Option<u32> {
        let px = (sp.x - self.rect.min.x) as i32;
        let py = (sp.y - self.rect.min.y) as i32;
        for rad in 0..=r.max(0) {
            for dy in -rad..=rad {
                for dx in -rad..=rad {
                    if dx.abs().max(dy.abs()) != rad {
                        continue; // interior already scanned by a smaller ring
                    }
                    if let Some(f) = pick_id_to_face(self.id_at_px(px + dx, py + dy)) {
                        return Some(f);
                    }
                }
            }
        }
        None
    }
}

/// An in-flight select-buffer readback.
pub(crate) struct PolyPickPending {
    pub map_ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub rect: egui::Rect,
    pub w: u32,
    pub h: u32,
    pub cam_key: u64,
    pub mesh_key: u64,
}

impl AzraApp {
    /// Key for "the select buffer must be re-rendered": mesh content + **modifier stack** +
    /// world transform + hidden set (POLY_VIEWPORT_PERF P5 / C16).
    /// Also reused by `poly_hover_pick` as the CPU pick-cache invalidation key — the cache must
    /// rebuild on the SAME state changes (mesh edit / transform / hide / stack), not only camera.
    pub(crate) fn poly_pick_mesh_key(&self) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let nid = self.active_poly_nid()?;
        let node = self.scene.scene_graph.get(nid)?;
        let NodePayload::PolygonalBody { mesh, modifiers, .. } = &node.payload else {
            return None;
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        // Same identity as solid display skip key (content + stack + boolean cutters).
        let (csig, stack_key) = self.poly_display_cache_key(nid, mesh, modifiers);
        csig.hash(&mut h);
        stack_key.hash(&mut h);
        for c in self.scene.scene_graph.world_transform(nid).to_cols_array() {
            c.to_bits().hash(&mut h);
        }
        // Hidden verts skip incident faces in the pick mesh (neither occlude nor select).
        let mut hidden: Vec<u32> = self.poly_sel.hidden_vertices.iter().copied().collect();
        hidden.sort_unstable();
        hidden.hash(&mut h);
        Some(h.finish())
    }

    /// The current (cam_key, mesh_key) the select buffer should have, or None when not needed.
    ///
    /// Built for poly **edit** sub-modes (selection) and also when polygroup colour overlay is on —
    /// the overlay uses the same face-ID image for depth occlusion (hidden faces must not paint
    /// through the solid mesh on MatCap).
    fn poly_pick_wanted_keys(&self) -> Option<(egui::Rect, u64, u64)> {
        if self.modeling_context != crate::ModelingContext::Polygonal { return None; }
        let need_for_edit = self.poly_work_mode != crate::PolyWorkMode::Object;
        let need_for_polygroups = self.ui.poly_show_polygroups;
        if !need_for_edit && !need_for_polygroups { return None; }
        let rect = self.viewport.viewport_rect;
        if rect.width() < 32.0 || rect.height() < 32.0 { return None; }
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let cam_key = crate::poly_selection::pick_cam_key(cam_pos, self.viewport.camera.orbit, rect, ortho_hw);
        let mesh_key = self.poly_pick_mesh_key()?;
        Some((rect, cam_key, mesh_key))
    }

    /// True when face `fid` is the frontmost surface at viewport point `sp` (not occluded).
    /// Uses the GPU pick buffer when valid; otherwise `None` (caller falls back to CPU).
    pub(crate) fn poly_face_visible_at_screen(
        &self,
        fid: u32,
        sp: egui::Pos2,
    ) -> Option<bool> {
        let img = self.poly_pick_image_valid()?;
        // Own tagged id; neighborhood absorbs raster edge slop (boundary pixels may carry the
        // face's rim EDGE/VERT ids now, but a 3×3 window always reaches interior pixels of any
        // face wider than ~2px — thinner faces are covered by their rim in the callers).
        Some(img.any_near(sp, |id| id == pick_id_face(fid)))
    }

    /// Borrow the select buffer IF it is valid for the current camera + mesh state.
    pub(crate) fn poly_pick_image_valid(&self) -> Option<&PolyPickImage> {
        let (_, cam_key, mesh_key) = self.poly_pick_wanted_keys()?;
        let img = self.poly_pick_image.as_ref()?;
        (img.cam_key == cam_key && img.mesh_key == mesh_key).then_some(img)
    }

    /// Poll phase: complete an in-flight readback and/or kick a refresh when the buffer is stale.
    /// Called from poll_background_tasks (the only place the GPU work happens — Frame Ordering).
    ///
    /// `want_sync` = a pointer button was PRESSED this frame in a poly edit context: the click
    /// gesture that follows (hover gate → release select / region apply / paint stroke) will ask
    /// visibility questions, so a stale buffer is refreshed SYNCHRONOUSLY right now — Blender
    /// renders its select buffer inside the pick operator for exactly this reason. Presses are
    /// discrete (one blocking render per click, never per frame), so the cost is a click-latency
    /// tax of a few ms, not a frame-rate tax.
    pub(crate) fn poll_poly_select_buffer(&mut self, want_sync: bool) {
        // 1. Complete a pending readback (blocking when a click needs it this frame).
        if let Some(pending) = &self.poly_pick_pending {
            let Some(gpu) = &self.gpu else { self.poly_pick_pending = None; return };
            let ids = if want_sync {
                gpu.picking.blocking_readback_full(&gpu.device, &pending.map_ready)
            } else {
                gpu.picking.try_readback_full(&gpu.device, &pending.map_ready)
            };
            if let Some(ids) = ids {
                self.poly_pick_image = Some(PolyPickImage {
                    rect: pending.rect,
                    w: pending.w,
                    h: pending.h,
                    ids,
                    cam_key: pending.cam_key,
                    mesh_key: pending.mesh_key,
                });
                self.poly_pick_pending = None;
            } else {
                return; // still mapping — single readback in flight, try next frame
            }
        }

        // NOT WHILE A DRAG EPISODE IS OPEN (VIEWPORT_PERF_2026-08-12 item E: `[SLOWPOLL]
        // polypick=44` on alternating frames of a drag). The mesh-settle latch below compares
        // against the PREVIOUS frame only, so any edit whose mesh key lands on alternating frames
        // reads settled-stale-settled and the whole ID pass + 8 MB readback ran every other frame.
        // An open episode is the architecture's own statement that a gesture is mid-flight and
        // will commit: the mesh is being mutated and no selection query can be answered against it
        // until it stops. The refresh happens on the first poll after the commit. This cannot wedge
        // the buffer (selection is P0) — `auto_commit_episodes` closes a leaked episode at the end
        // of every frame — and selection itself opens no episode, so paint / region select keep it.
        //
        // Bail BEFORE `poly_pick_wanted_keys` while a drag episode is open — computing the keys is
        // itself the cost being avoided. `poly_pick_mesh_key` → `poly_display_cache_key` is memoed
        // on `poly_edit_generation`, and a live edit bumps that generation EVERY frame, so the memo
        // misses every frame and the full `content_signature` mesh walk runs (~6 ms at 160k faces,
        // more at 323k) purely to decide that the buffer is stale — which the open episode already
        // told us. A pointer press still gets its synchronous refresh: `want_sync` is the frame the
        // click needs an answer, and that answer must not depend on what else is in flight.
        if !want_sync && self.pending_episode.is_some() { return; }
        let Some((rect, cam_key, mesh_key)) = self.poly_pick_wanted_keys() else { return };
        let cam_settled = self.poly_pick_last_cam_key == cam_key;
        let mesh_settled = self.poly_pick_last_mesh_key == mesh_key;
        self.poly_pick_last_cam_key = cam_key;
        self.poly_pick_last_mesh_key = mesh_key;
        if self.poly_pick_image.as_ref().map_or(false, |i| i.cam_key == cam_key && i.mesh_key == mesh_key) {
            return; // up to date
        }
        // 2. Click pressed on a stale buffer → render + block for the image NOW (no settle gate:
        //    the press defines the state the click will be interpreted against).
        if want_sync {
            self.render_poly_select_buffer(rect, cam_key, mesh_key, true);
            return;
        }
        // 3. Idle refresh when stale — but only once BOTH the camera AND the mesh have settled
        //    (unchanged since last frame). Camera-settle avoids an 8MB readback per orbit frame;
        //    mesh-settle avoids re-running the whole ID pass + readback on EVERY frame of a live
        //    edit (a bevel/extrude/inset drag mutates the mesh each frame, so `mesh_key` never
        //    matches the cached image).
        if !cam_settled || !mesh_settled { return; }
        self.render_poly_select_buffer(rect, cam_key, mesh_key, false);
    }

    /// Encode + submit the per-element ID render and start the full readback
    /// (async, or blocking when `block` — the click-time sync path).
    ///
    /// POLY_VIEWPORT_PERF P5 / C16:
    /// - Triangulation = **`earclip_polygon`** — same as solid `raster_data_ngon` (not fan).
    /// - Geometry = **base cage** with base ids (edit selection is always base indices).
    ///   When no geometry modifiers are enabled this matches the solid. With Subdivide/Array
    ///   the solid is the display mesh; pick still selects the **edit cage** (correct ids).
    /// - VB is capacity-backed (`upload_poly_vertices`) — no `create_buffer_init` every settle.
    /// - ONE buffer, three ranges: [face tris + occluder tris][edge lines][vert points], drawn
    ///   in that order (see picking.rs bias ladder). Occluders = every OTHER visible poly body's
    ///   faces at id 0 — they write depth so a body in front genuinely occludes the active
    ///   body's elements (Blender draws non-edit geometry depth-only for the same reason).
    ///   Known gap, unchanged from before: SDF / voxel bodies still do not occlude poly picks.
    fn render_poly_select_buffer(&mut self, rect: egui::Rect, cam_key: u64, mesh_key: u64, block: bool) {
        let Some(nid) = self.active_poly_nid() else {
            return;
        };
        let (verts, n_face_verts, n_edge_verts) = {
            let Some(node) = self.scene.scene_graph.get(nid) else {
                return;
            };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                return;
            };
            let world = self.scene.scene_graph.world_transform(nid);
            let hidden = &self.poly_sel.hidden_vertices;
            let mut out: Vec<PolyPickVertex> =
                Vec::with_capacity(mesh.faces.len().saturating_mul(6));
            // Per-face triangulation in PARALLEL (VIEWPORT_PERF_2026-08-12 item E). This walk is
            // the 44 ms in `[SLOWPOLL] polypick`: at 323k faces it is three Vec allocations and an
            // ear-clip per face, and it runs on the frame thread. Gating it out of drags (see
            // `poll_poly_select_buffer` step 4) removed the per-frame cost; splitting it across
            // cores shrinks the once-per-settle hitch that remains. Same cost class, and the same
            // remedy, as the 2026-08-08 poly display-bake fix.
            //
            // `map` + ordered `collect` keeps face order deterministic. Order does not affect
            // correctness here (the pass is depth-resolved), but a stable vertex buffer keeps the
            // headless pick tests reproducible.
            fn push_faces(out: &mut Vec<PolyPickVertex>,
                          mesh: &core_math::half_edge::HalfEdgeMesh,
                          world: glam::Mat4,
                          hidden: &std::collections::HashSet<u32>,
                          tagged: bool) {
                use rayon::prelude::*;
                let per_face: Vec<Vec<PolyPickVertex>> = (0..mesh.faces.len() as u32)
                    .into_par_iter()
                    .map(|fi| {
                        let mut tris: Vec<PolyPickVertex> = Vec::new();
                        let fid = FaceId(fi);
                        if mesh.face(fid).half_edge == NO_EDGE {
                            return tris;
                        }
                        let ring = mesh.face_vertices(fid);
                        if ring.len() < 3 {
                            return tris;
                        }
                        if ring.iter().any(|v| hidden.contains(&v.0)) {
                            return tris; // hidden: no draw, no occlude
                        }
                        let face_n = mesh.compute_face_normal(fid);
                        let local_pts: Vec<Vec3> =
                            ring.iter().map(|&v| mesh.vert(v).position).collect();
                        // Same ear-clip as solid raster (poly_mesh.rs) — concave-safe vs a fan.
                        let id = if tagged { pick_id_face(fi) } else { 0 };
                        for tri in core_math::poly_mesh::earclip_polygon(&local_pts, face_n) {
                            for &ci in &tri {
                                let p = world.transform_point3(local_pts[ci as usize]);
                                tris.push(PolyPickVertex { pos: [p.x, p.y, p.z], id });
                            }
                        }
                        tris
                    })
                    .collect();
                out.reserve(per_face.iter().map(|v| v.len()).sum());
                for v in per_face {
                    out.extend_from_slice(&v);
                }
            }
            // Active body's faces with real ids…
            push_faces(&mut out, mesh, world, hidden, true);
            // …then every OTHER visible poly body as an id-0 depth occluder (hidden state is
            // per-active-body; other bodies hide nothing — same rule as the CPU face pick).
            let no_hidden: std::collections::HashSet<u32> = std::collections::HashSet::new();
            for (bnid, bnode) in self.scene.scene_graph.poly_bodies() {
                if bnid == nid || !bnode.visible {
                    continue;
                }
                let NodePayload::PolygonalBody { mesh: bmesh, .. } = &bnode.payload else {
                    continue;
                };
                let bworld = self.scene.scene_graph.world_transform(bnid);
                push_faces(&mut out, bmesh, bworld, &no_hidden, false);
            }
            let n_face_verts = out.len();
            // Edge lines — one segment per canonical (min-of-twin-pair) live half-edge. Wire and
            // boundary edges included: they are real selectable segments (same rule as the cache).
            for he_id in mesh.canonical_edges() {
                let hei = mesh.he(he_id);
                let src = mesh.he_source(he_id);
                if hidden.contains(&src.0) || hidden.contains(&hei.vertex.0) {
                    continue;
                }
                let id = pick_id_edge(he_id.0);
                let p0 = world.transform_point3(mesh.vert(src).position);
                let p1 = world.transform_point3(mesh.vert(hei.vertex).position);
                out.push(PolyPickVertex { pos: [p0.x, p0.y, p0.z], id });
                out.push(PolyPickVertex { pos: [p1.x, p1.y, p1.z], id });
            }
            let n_edge_verts = out.len() - n_face_verts;
            // Vert points — 1px each; visibility probes use their OWN id so 1px is enough.
            for i in 0..mesh.vertices.len() as u32 {
                if mesh.vert(VertexId(i)).half_edge == NO_EDGE || hidden.contains(&i) {
                    continue;
                }
                let p = world.transform_point3(mesh.vert(VertexId(i)).position);
                out.push(PolyPickVertex { pos: [p.x, p.y, p.z], id: pick_id_vert(i) });
            }
            (out, n_face_verts, n_edge_verts)
        };
        if verts.is_empty() {
            return;
        }

        let Some(gpu) = &mut self.gpu else {
            return;
        };
        let device = &gpu.device;
        let queue = &gpu.queue;
        let (w, h) = (rect.width() as u32, rect.height() as u32);

        gpu.picking.ensure_textures(device, w, h);
        gpu.picking.ensure_full_staging(device);
        let (near, far) =
            render::camera::orbit_clip_planes(self.viewport.camera.distance, self.render.ortho);
        let camera = CameraUniforms::from_orbit(
            self.viewport.camera.orbit,
            self.viewport.camera.distance,
            self.viewport.camera.target,
            w as f32 / h.max(1) as f32,
            self.render.fov_deg,
            near,
            far,
            self.render.ortho,
        );
        gpu.picking.upload_camera(queue, &camera);
        if gpu.picking.bind_group.is_none() {
            gpu.picking.rebuild_bind_group(device);
        }
        // Element pass needs camera + the face-depth view; `ensure_textures` rebuilds it on
        // every texture recreation, this covers the first-use case.
        if gpu.picking.poly_elem_bind_group.is_none() {
            gpu.picking.rebuild_poly_elem_bind_group(device);
        }

        let vcount = gpu.picking.upload_poly_vertices(device, queue, &verts);
        let nf = (n_face_verts as u32).min(vcount);
        let ne = ((n_face_verts + n_edge_verts) as u32).min(vcount);

        use eframe::wgpu;
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("poly_pick"),
        });
        // Two-pass encode (faces → elements) lives in render::picking so the headless
        // regression tests drive the SAME wiring the app ships.
        gpu.picking.encode_poly_select(&mut enc, nf, ne, vcount);
        gpu.picking.copy_full(&mut enc);
        queue.submit(Some(enc.finish()));
        let map_ready = gpu.picking.start_full_readback();
        if block {
            // Click-time sync path: wait for the map and install the image THIS frame, so the
            // hover gate + release select that follow in this same frame see fresh visibility.
            if let Some(ids) = gpu.picking.blocking_readback_full(&gpu.device, &map_ready) {
                self.poly_pick_image = Some(PolyPickImage { rect, w, h, ids, cam_key, mesh_key });
                self.poly_pick_pending = None;
                return;
            }
            // Map never completed (device trouble) — fall through to the async pending path.
        }
        self.poly_pick_pending = Some(PolyPickPending {
            map_ready,
            rect,
            w,
            h,
            cam_key,
            mesh_key,
        });
    }
}
