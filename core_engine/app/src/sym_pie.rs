// sym_pie.rs — Alt+X Symmetry pie (poly), owner's design.
//
// The pie itself is the shared `self.ui.sym_pie: PieMenuState` (drawn by `pie_menu::draw_pie_menu`
// with `pie_menu::sym_pie_def()`). This module holds the tool params + the open / tick / apply glue.
//
// DESIGN: Alt+X = quick Symmetrize pie AT THE CURSOR. No viewport plane gizmo, no toast spam.
// Select edge/face → Alt+X → flick = symmetrize from that selection. No selection → geometry center.
// Keys (optional): o/p/g center, c cut, i instance, m modifier. Alt+X / Esc / RMB close.

use eframe::egui;
use glam::Vec3;
use core_math::half_edge::{FaceId, HalfEdgeId, NO_FACE, NO_TWIN};
use core_math::scene_graph::{NodeId, NodePayload};
use crate::{AzraApp, world_to_screen};

/// Flick distance (px from center) at which a hovered direction auto-confirms a Mirror.
const SYM_PIE_CONFIRM_DIST: f32 = 106.0;

/// Center reference for the mirror plane.
#[derive(Clone, Copy, PartialEq, Default)]
pub(crate) enum SymCenter {
    /// Bounding-box center of the mesh — "center of the volume" (default, owner's key ask).
    #[default]
    Geometry,
    Origin, // world origin (0,0,0)
    Pivot,  // object pivot = body local origin
    /// From selected face(s) or edge(s) at pie open.
    Selection,
}

/// Which operation a confirmed direction runs.
#[derive(Clone, Copy, PartialEq)]
enum SymMode { Mirror, Cut, Instance, Modifier }

/// Non-pie tool params (the pie visual/interaction lives in `self.ui.sym_pie: PieMenuState`).
#[derive(Clone, Default)]
pub(crate) struct SymPieParams {
    pub center: SymCenter,
    /// PolygonalBody captured at open — the op targets THIS node even if selection changes.
    pub nid: Option<NodeId>,
    /// Selection-defined plane in mesh LOCAL space: (origin, unit normal).
    /// When set, overrides the X/Y/Z flick axis for plane orientation (flick still picks kept side).
    pub selection_plane: Option<(Vec3, Vec3)>,
}

/// Screen directions for the 4 slices, matching `sector_center_angle(i, 4)`:
/// 0 = up, 1 = right, 2 = down, 3 = left (egui screen space: +y is down).
const SLICE_SCREEN_DIRS: [egui::Vec2; 4] = [
    egui::vec2(0.0, -1.0),
    egui::vec2(1.0, 0.0),
    egui::vec2(0.0, 1.0),
    egui::vec2(-1.0, 0.0),
];

impl AzraApp {
    pub(crate) fn active_poly_nid(&self) -> Option<NodeId> {
        let n = self.sel.usel.active_node()?;
        match self.scene.scene_graph.get(n).map(|x| &x.payload) {
            Some(NodePayload::PolygonalBody { .. }) => Some(n),
            _ => None,
        }
    }

    /// Open the Alt+X quick Symmetrize pie at the cursor.
    /// No viewport plane gizmo / symmetry UI — pie only.
    /// If faces or edges are selected, the mirror plane is taken from them silently.
    pub(crate) fn open_sym_pie(&mut self, mouse: egui::Pos2) {
        let nid = match self.active_poly_nid() {
            Some(n) => n,
            None => { self.toast("Select a polygon mesh first"); return; }
        };
        // Anchor safety net: if the pointer position is unknown (~origin), open at the viewport center
        // rather than the top-left corner.
        let anchor = if mouse.x <= 1.0 && mouse.y <= 1.0 { self.viewport.viewport_rect.center() } else { mouse };

        // Selection → plane (no viewport gizmo). Pie alone does the rest.
        let selection_plane = self.poly_sym_plane_from_selection(nid);
        let center = if selection_plane.is_some() {
            SymCenter::Selection
        } else {
            SymCenter::Geometry
        };

        self.ui.sym_pie_params = SymPieParams {
            center,
            nid: Some(nid),
            selection_plane,
        };
        self.ui.z_pie.open = false;
        self.ui.tab_pie.open = false;
        self.ui.f_pie.open = false;
        self.ui.sym_pie.open(anchor, 4);
        // No toast, no plane visual — quick flick symmetrize only.
    }

    pub(crate) fn sym_center_label(&self) -> &'static str {
        match self.ui.sym_pie_params.center {
            SymCenter::Geometry => "GEOMETRY",
            SymCenter::Origin => "ORIGIN",
            SymCenter::Pivot => "PIVOT",
            SymCenter::Selection => "SELECTION",
        }
    }

    /// Derive a local-space mirror plane from poly selection (faces preferred, else edges).
    /// - **Face(s):** plane = average face center + average face normal (selected polygon(s)).
    /// - **Edge(s):** plane CONTAINS the selected edges (seam-style). Origin = average midpoints;
    ///   normal from adjacent-face bisector / multi-edge span (NOT "edge direction as normal").
    ///   Flick still only chooses which side of that plane to keep.
    pub(crate) fn poly_sym_plane_from_selection(&self, nid: NodeId) -> Option<(Vec3, Vec3)> {
        let node = self.scene.scene_graph.get(nid)?;
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh.as_ref(),
            _ => return None,
        };

        // Faces first when any face is selected (explicit face plane).
        if !self.poly_sel.selected_faces.is_empty() {
            let faces: Vec<u32> = self.poly_sel.selected_faces.iter().copied().collect();
            let mut o = Vec3::ZERO;
            let mut n = Vec3::ZERO;
            let mut count = 0u32;
            for &fid in &faces {
                let f = FaceId(fid);
                if f.0 as usize >= mesh.faces.len() {
                    continue;
                }
                if mesh.face(f).half_edge.0 as usize >= mesh.half_edges.len() {
                    continue;
                }
                let vs = mesh.face_vertices(f);
                if vs.len() < 3 {
                    continue;
                }
                let c: Vec3 = vs.iter().map(|&v| mesh.vert(v).position).sum::<Vec3>()
                    / vs.len() as f32;
                let fnorm = mesh.compute_face_normal(f);
                if fnorm.length_squared() < 1e-8 {
                    continue;
                }
                o += c;
                n += fnorm;
                count += 1;
            }
            if count > 0 {
                let n = n.normalize_or_zero();
                if n.length_squared() > 0.25 {
                    return Some((o / count as f32, n));
                }
            }
        }

        self.poly_sym_plane_from_edges(mesh)
    }

    /// Build a mirror plane that has the selected edges lying on it (center-seam workflow).
    ///
    /// 1. Origin = average of edge midpoints.
    /// 2. Prefer multi-edge span: normal from cross products of edge directions / point spreads.
    /// 3. Else single edge: normal from adjacent face normals (n0 − n1), projected ⊥ edge dir.
    /// 4. Last resort: edge direction itself as normal (plane ⊥ edge through midpoint).
    fn poly_sym_plane_from_edges(
        &self,
        mesh: &core_math::half_edge::HalfEdgeMesh,
    ) -> Option<(Vec3, Vec3)> {
        if self.poly_sel.selected_edges.is_empty() {
            return None;
        }

        struct EdgeSample {
            mid: Vec3,
            dir: Vec3,
            pa: Vec3,
            pb: Vec3,
            he: HalfEdgeId,
        }

        let mut samples: Vec<EdgeSample> = Vec::new();
        for &eid in &self.poly_sel.selected_edges {
            let he = HalfEdgeId(eid);
            if !mesh.is_edge_live(he) {
                continue;
            }
            let a = mesh.he_source(he);
            let b = mesh.he(he).vertex;
            if a.0 as usize >= mesh.vertices.len() || b.0 as usize >= mesh.vertices.len() {
                continue;
            }
            let pa = mesh.vert(a).position;
            let pb = mesh.vert(b).position;
            let d = pb - pa;
            if d.length_squared() < 1e-12 {
                continue;
            }
            samples.push(EdgeSample {
                mid: (pa + pb) * 0.5,
                dir: d.normalize(),
                pa,
                pb,
                he,
            });
        }
        if samples.is_empty() {
            return None;
        }

        let origin = samples.iter().map(|s| s.mid).sum::<Vec3>() / samples.len() as f32;

        // --- Multi-edge / multi-point span: plane that contains the edges as well as possible ---
        // Accumulate cross products of (aligned) edge dirs and of vectors between midpoints.
        let mut n_acc = Vec3::ZERO;
        let dir_ref = samples[0].dir;
        for s in &samples {
            let d = if s.dir.dot(dir_ref) < 0.0 { -s.dir } else { s.dir };
            // Cross with reference direction → normal when edges are not collinear.
            let c = dir_ref.cross(d);
            if c.length_squared() > 1e-8 {
                let cu = c.normalize();
                if n_acc.length_squared() > 0.0 && n_acc.dot(cu) < 0.0 {
                    n_acc -= cu;
                } else {
                    n_acc += cu;
                }
            }
            // Cross of midpoint offsets with edge dir (edges that form a polyline).
            let off = s.mid - origin;
            if off.length_squared() > 1e-10 {
                let c2 = off.cross(d);
                if c2.length_squared() > 1e-8 {
                    let cu = c2.normalize();
                    if n_acc.length_squared() > 0.0 && n_acc.dot(cu) < 0.0 {
                        n_acc -= cu;
                    } else {
                        n_acc += cu;
                    }
                }
            }
        }
        // Also use all endpoints relative to origin (robust for a selected edge loop / ring).
        for s in &samples {
            for p in [s.pa, s.pb] {
                let off = p - origin;
                if off.length_squared() < 1e-10 {
                    continue;
                }
                // Cross successive endpoint offsets with the first edge dir for a stable basis.
                let c = off.cross(dir_ref);
                if c.length_squared() > 1e-8 {
                    let cu = c.normalize();
                    if n_acc.length_squared() > 0.0 && n_acc.dot(cu) < 0.0 {
                        n_acc -= cu;
                    } else {
                        n_acc += cu;
                    }
                }
            }
        }
        let n_span = n_acc.normalize_or_zero();
        if n_span.length_squared() > 0.25 {
            return Some((origin, n_span));
        }

        // --- Single edge / collinear: use adjacent faces so the plane CONTAINS the edge ---
        let mut face_n_diff = Vec3::ZERO;
        let mut face_hits = 0u32;
        for s in &samples {
            let he = s.he;
            let face0 = mesh.he(he).face;
            let twin = mesh.he(he).twin;
            let face1 = if twin != NO_TWIN && (twin.0 as usize) < mesh.half_edges.len() {
                mesh.he(twin).face
            } else {
                NO_FACE
            };

            let mut n0 = Vec3::ZERO;
            let mut n1 = Vec3::ZERO;
            let mut have0 = false;
            let mut have1 = false;
            if face0 != NO_FACE && (face0.0 as usize) < mesh.faces.len() {
                let n = mesh.compute_face_normal(face0);
                if n.length_squared() > 1e-8 {
                    n0 = n.normalize();
                    have0 = true;
                }
            }
            if face1 != NO_FACE && (face1.0 as usize) < mesh.faces.len() {
                let n = mesh.compute_face_normal(face1);
                if n.length_squared() > 1e-8 {
                    n1 = n.normalize();
                    have1 = true;
                }
            }

            if have0 && have1 {
                // Difference points across the dihedral — good center-seam normal after ⊥ edge proj.
                let mut d = n0 - n1;
                if d.length_squared() < 1e-8 {
                    // Nearly coplanar faces: stand the plane up from the face using edge × face_n.
                    d = s.dir.cross(n0);
                }
                // Project onto plane perpendicular to edge so the edge lies in the mirror plane.
                d = d - s.dir * d.dot(s.dir);
                if d.length_squared() > 1e-8 {
                    let du = d.normalize();
                    if face_hits > 0 && face_n_diff.dot(du) < 0.0 {
                        face_n_diff -= du;
                    } else {
                        face_n_diff += du;
                    }
                    face_hits += 1;
                }
            } else if have0 || have1 {
                let nf = if have0 { n0 } else { n1 };
                // One-sided: plane contains edge (edge as hinge), normal = edge × face_n.
                let mut d = s.dir.cross(nf);
                d = d - s.dir * d.dot(s.dir);
                if d.length_squared() > 1e-8 {
                    let du = d.normalize();
                    if face_hits > 0 && face_n_diff.dot(du) < 0.0 {
                        face_n_diff -= du;
                    } else {
                        face_n_diff += du;
                    }
                    face_hits += 1;
                }
            }
        }
        let n_face = face_n_diff.normalize_or_zero();
        if n_face.length_squared() > 0.25 {
            return Some((origin, n_face));
        }

        // --- Last resort: plane ⊥ edge (old midplane) ---
        let mut dir_sum = Vec3::ZERO;
        for (i, s) in samples.iter().enumerate() {
            let d = if i > 0 && dir_sum.dot(s.dir) < 0.0 { -s.dir } else { s.dir };
            dir_sum += d;
        }
        let n = dir_sum.normalize_or_zero();
        if n.length_squared() < 0.25 {
            return None;
        }
        Some((origin, n))
    }

    /// Per-frame modal tick while the pie is open (handle_keyboard returns early to run this).
    pub(crate) fn tick_sym_pie(&mut self, ctx: &egui::Context, mouse: egui::Pos2) {
        let def = crate::pie_menu::sym_pie_def(self.sym_center_label());
        self.ui.sym_pie.update_hover(mouse, &def, 1.0);
        self.ui.sym_pie.tick(ctx.input(|i| i.stable_dt));

        let (o, p, c, i_key, m, alt_x, esc, rmb, lmb) = ctx.input(|inp| (
            inp.key_pressed(egui::Key::O),
            inp.key_pressed(egui::Key::P),
            inp.key_pressed(egui::Key::C),
            inp.key_pressed(egui::Key::I),
            inp.key_pressed(egui::Key::M),
            // Alt+X — the SAME chord that opened it — toggles it closed. (Bare X does nothing: it is
            // not part of this tool's design.)
            inp.key_pressed(egui::Key::X) && inp.modifiers.alt,
            inp.key_pressed(egui::Key::Escape),
            inp.pointer.button_pressed(egui::PointerButton::Secondary),
            inp.pointer.button_pressed(egui::PointerButton::Primary),
        ));

        // Cancels: Alt+X / Esc / RMB — silent (no gizmo, no cancel toast).
        if alt_x || esc || rmb {
            self.ui.sym_pie.open = false;
            return;
        }
        // Optional center overrides (no toast). Leaving SELECTION clears the selection plane.
        if o {
            self.ui.sym_pie_params.center = SymCenter::Origin;
            self.ui.sym_pie_params.selection_plane = None;
        }
        if p {
            self.ui.sym_pie_params.center = SymCenter::Pivot;
            self.ui.sym_pie_params.selection_plane = None;
        }
        if ctx.input(|inp| inp.key_pressed(egui::Key::G)) {
            self.ui.sym_pie_params.center = SymCenter::Geometry;
            self.ui.sym_pie_params.selection_plane = None;
        }

        let hovered = self.ui.sym_pie.hovered_inner;
        let dist = (mouse - self.ui.sym_pie.center).length();

        // c / i / m apply immediately in the pointed direction.
        if let Some(idx) = hovered {
            if c     { self.ui.sym_pie.open = false; self.apply_sym_pie(idx, SymMode::Cut);      return; }
            if i_key { self.ui.sym_pie.open = false; self.apply_sym_pie(idx, SymMode::Instance); return; }
            if m     { self.ui.sym_pie.open = false; self.apply_sym_pie(idx, SymMode::Modifier); return; }
            // Flick-out = Mirror (no click, no hold).
            if dist > SYM_PIE_CONFIRM_DIST {
                self.ui.sym_pie.open = false;
                self.apply_sym_pie(idx, SymMode::Mirror);
                return;
            }
        }

        // Left-click confirms Mirror on a slice; click center/empty closes silently.
        if lmb {
            self.ui.sym_pie.open = false;
            self.ui.pie_eat_click = true; // this click must not also reach viewport input this frame
            if let Some(idx) = hovered {
                self.apply_sym_pie(idx, SymMode::Mirror);
            }
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }

    /// Apply `mode` across the plane for slice `idx` (its screen direction) on the captured node.
    fn apply_sym_pie(&mut self, idx: usize, mode: SymMode) {
        let params = self.ui.sym_pie_params.clone();
        let nid = match params.nid { Some(n) => n, None => return };
        let dir = SLICE_SCREEN_DIRS.get(idx).copied().unwrap_or(egui::vec2(1.0, 0.0));
        let (origin, normal, axis) = match self.sym_resolve(nid, params.center, dir) {
            Some(v) => v,
            None => { self.toast("Symmetry: could not resolve axis"); return; }
        };
        match mode {
            SymMode::Mirror   => self.poly_symmetrize_op(nid, origin, normal, false),
            SymMode::Cut      => self.poly_symmetrize_op(nid, origin, normal, true),
            SymMode::Instance => self.poly_mirror_instance(nid, origin, normal),
            SymMode::Modifier => self.poly_add_mirror_modifier(nid, axis),
        }
    }

    /// Resolve center + flicked screen direction into (origin_local, normal_local, axis_u8).
    /// `normal_local` points at the KEPT (source) side — symmetrize keeps the +normal half and mirrors
    /// it onto the flick DESTINATION side ("flick right = copy left onto right").
    ///
    /// When `selection_plane` is set (face/edge at Alt+X open), that plane is used; flick only
    /// chooses which side of it to keep (flip normal if needed).
    fn sym_resolve(&self, nid: NodeId, center: SymCenter, screen_dir: egui::Vec2) -> Option<(Vec3, Vec3, u8)> {
        let (bmin, bmax) = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { mesh, .. }) => mesh.bounding_box(),
            _ => return None,
        };
        let world = self.scene.scene_graph.world_transform(nid);
        let inv = world.inverse();

        // --- Selection-defined plane (faces / edges) ---
        if let Some((o_sel, n_sel)) = self.ui.sym_pie_params.selection_plane {
            if n_sel.length_squared() > 0.25 {
                let origin_local = match center {
                    SymCenter::Selection => o_sel,
                    SymCenter::Geometry => (bmin + bmax) * 0.5,
                    SymCenter::Origin => inv.transform_point3(Vec3::ZERO),
                    SymCenter::Pivot => Vec3::ZERO,
                };
                // Origin on the selection plane when using Geometry/Origin/Pivot: project onto plane.
                let origin_local = if matches!(center, SymCenter::Selection) {
                    origin_local
                } else {
                    // Keep origin on selection plane: o + n*((p-o)·n) wait project p onto plane
                    let d = (origin_local - o_sel).dot(n_sel);
                    origin_local - n_sel * d
                };
                let origin_world = world.transform_point3(origin_local);
                let n_world = world.transform_vector3(n_sel).normalize_or_zero();
                if n_world.length_squared() < 0.5 {
                    return None;
                }
                // Flick picks kept side: keep side opposite the flick (same as axis path).
                let dest_hint = self.sym_flick_world_dir(origin_world, screen_dir)?;
                let mut normal_local = n_sel;
                // If +n points toward the flick destination, flip so +n is the source (kept) side.
                let n_to_dest = n_world.dot(dest_hint);
                if n_to_dest > 0.0 {
                    normal_local = -normal_local;
                }
                let a = dest_hint.abs();
                let axis = if a.x >= a.y && a.x >= a.z {
                    0u8
                } else if a.y >= a.z {
                    1
                } else {
                    2
                };
                return Some((origin_local, normal_local, axis));
            }
        }

        let origin_local = match center {
            SymCenter::Geometry | SymCenter::Selection => (bmin + bmax) * 0.5,
            SymCenter::Origin => inv.transform_point3(Vec3::ZERO),
            SymCenter::Pivot => Vec3::ZERO,
        };
        let origin_world = world.transform_point3(origin_local);

        let dest_world = self.sym_flick_world_dir(origin_world, screen_dir)?;
        // Keep the side OPPOSITE the flick (the source); mirror it onto the flick side.
        let normal_world = -dest_world;
        // World->local plane normal: inverse-transpose = transpose of the world linear part.
        let normal_local = world.transpose().transform_vector3(normal_world).normalize_or_zero();
        if normal_local.length_squared() < 0.5 {
            return None;
        }
        // Dominant world axis of the flick (for the Mirror modifier: 0=X,1=Y,2=Z).
        let a = dest_world.abs();
        let axis = if a.x >= a.y && a.x >= a.z {
            0u8
        } else if a.y >= a.z {
            1
        } else {
            2
        };
        Some((origin_local, normal_local, axis))
    }

    /// Map a screen flick to the best matching world ±X/Y/Z direction from `origin_world`.
    fn sym_flick_world_dir(&self, origin_world: Vec3, screen_dir: egui::Vec2) -> Option<Vec3> {
        if screen_dir.length() < 1e-4 {
            return None;
        }
        let mdir = screen_dir.normalized();
        let rect = self.viewport.viewport_rect;
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };
        let orbit = self.viewport.camera.orbit;
        let fov = self.render.fov_deg;
        let s0 = world_to_screen(origin_world, cam_pos, orbit, rect, aspect, ortho_hw, fov)?;
        let scale = self.viewport.camera.distance.max(0.001) * 0.25;

        let axes = [
            Vec3::X,
            Vec3::NEG_X,
            Vec3::Y,
            Vec3::NEG_Y,
            Vec3::Z,
            Vec3::NEG_Z,
        ];
        let mut best: Option<Vec3> = None;
        let mut best_dot = -2.0f32;
        for a in axes {
            if let Some(s1) =
                world_to_screen(origin_world + a * scale, cam_pos, orbit, rect, aspect, ortho_hw, fov)
            {
                let sd = egui::vec2(s1.x - s0.x, s1.y - s0.y);
                if sd.length() < 1.0 {
                    continue;
                }
                let d = sd.normalized().dot(mdir);
                if d > best_dot {
                    best_dot = d;
                    best = Some(a);
                }
            }
        }
        best
    }
}
