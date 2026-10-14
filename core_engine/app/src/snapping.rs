//! Snap candidate computation — shared by smart snap and symmetry tools.
//!
//! SDF prim features are converted to **world** space under `BODY_LOCAL_PRIMS`.
//! Poly meshes use `world_transform(nid)`. Dense poly candidates are capped.

use crate::*;
use glam::Vec3;

/// Soft cap per poly body so dense meshes do not flood screen-space nearest-neighbor.
const POLY_SNAP_MAX_VERTS: usize = 512;
const POLY_SNAP_MAX_EDGES: usize = 768;
const POLY_SNAP_MAX_FACES: usize = 256;

impl AzraApp {
    /// Prim feature in shape-local units (before prim scale) → world position.
    /// Respects body-local prim storage (`BODY_LOCAL_PRIMS`) and body TRS.
    #[inline]
    pub(crate) fn prim_feature_world(
        &self,
        prim_idx: usize,
        p: &core_math::primitives::SdfPrimitive,
        shape_local: Vec3,
    ) -> Vec3 {
        let s = p.scale;
        let rq = p.rotation_quat();
        // Prim-local (body-local when BODY_LOCAL_PRIMS): origin + R*(shape*scale)
        let local = p.position + rq * (shape_local * s);
        if BODY_LOCAL_PRIMS {
            self.scene
                .prim_body_map
                .get(prim_idx)
                .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
                .map(|bt| bt.local_to_world(local))
                .unwrap_or(local)
        } else {
            local
        }
    }

    /// Shape-local unit direction → world normal (rotation only, no scale/translation).
    #[inline]
    pub(crate) fn prim_feature_world_normal(
        &self,
        prim_idx: usize,
        p: &core_math::primitives::SdfPrimitive,
        shape_local_dir: Vec3,
    ) -> Vec3 {
        let rq = p.rotation_quat();
        let local = rq * shape_local_dir;
        let w = if BODY_LOCAL_PRIMS {
            self.scene
                .prim_body_map
                .get(prim_idx)
                .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
                .map(|bt| bt.rotation * local)
                .unwrap_or(local)
        } else {
            local
        };
        w.normalize_or_zero()
    }

    /// Grid spacing for lattice targets **and** transform quantize (single source of truth).
    #[inline]
    pub(crate) fn effective_snap_grid(&self) -> f32 {
        self.viewport.snap_grid.max(0.01)
    }

    /// Affects flags + free-drag grid quantize + angle-lock option.
    /// Returns `(move, rotate, scale, grid_quantize, angle_lock)`.
    pub(crate) fn snap_input_flags(&self) -> (bool, bool, bool, bool, bool) {
        let quantize = self.viewport.smart_snap_grid
            || matches!(self.viewport.snap_mode, SnapMode::Increment);
        (
            self.ui2.snap_affect_move,
            self.ui2.snap_affect_rotate,
            self.ui2.snap_affect_scale,
            quantize,
            self.ui2.snap_angle_lock,
        )
    }

    /// Blender "Align Rotation to Target": if snap is active and a hit carries a surface
    /// normal, overwrite `result.rotation` so local +Y points along that normal (Y-up).
    /// Returns true when rotation was written.
    pub(crate) fn apply_snap_align_rotation(
        &self,
        snap_active: bool,
        result: &mut crate::transform::TransformResult,
    ) -> bool {
        if !snap_active || !self.ui2.snap_align_rotation {
            return false;
        }
        let Some(n) = self
            .viewport
            .smart_snap_result
            .as_ref()
            .and_then(|h| h.normal)
        else {
            return false;
        };
        let n = n.normalize_or_zero();
        if n.length_squared() < 0.5 {
            return false;
        }
        // Match scatter / create: local +Y → surface normal.
        let q = glam::Quat::from_rotation_arc(glam::Vec3::Y, n);
        if !q.is_finite() {
            return false;
        }
        result.rotation = crate::transform::quat_to_euler_yxz_deg(q);
        true
    }

    /// Compute all snap candidates from scene primitives for the symmetry tool / smart snap.
    pub(crate) fn compute_snap_candidates(&self) -> Vec<SnapCandidate> {
        let mut out = Vec::new();
        for (i, p) in self.scene.primitives.iter().enumerate() {
            if !p.visible {
                continue;
            }
            let name = &p.name;
            let w = |local: Vec3| self.prim_feature_world(i, p, local);

            // Origin / center
            out.push(SnapCandidate::point(w(Vec3::ZERO), SnapFilter::Vertex, format!("{name} origin"), i));
            out.push(SnapCandidate::point(w(Vec3::ZERO), SnapFilter::All, format!("{name} center"), i));

            match &p.shape {
                SdfShape::Sphere { radius } => {
                    let r = *radius;
                    for (dir, lbl) in [
                        (Vec3::Y, "top"),
                        (Vec3::NEG_Y, "bottom"),
                        (Vec3::X, "+X"),
                        (Vec3::NEG_X, "-X"),
                        (Vec3::Z, "+Z"),
                        (Vec3::NEG_Z, "-Z"),
                    ] {
                        out.push(SnapCandidate::point(
                            w(dir * r),
                            SnapFilter::Vertex,
                            format!("{name} {lbl}"),
                            i,
                        ));
                    }
                }
                SdfShape::Box { half_extents, .. } => {
                    let he = *half_extents;
                    for sx in [-1.0f32, 1.0] {
                        for sy in [-1.0f32, 1.0] {
                            for sz in [-1.0f32, 1.0] {
                                out.push(SnapCandidate::point(
                                    w(Vec3::new(he.x * sx, he.y * sy, he.z * sz)),
                                    SnapFilter::Vertex,
                                    format!("{name} corner"),
                                    i,
                                ));
                            }
                        }
                    }
                    let edges: [(Vec3, Vec3); 12] = [
                        (Vec3::new(-he.x, -he.y, -he.z), Vec3::new(he.x, -he.y, -he.z)),
                        (Vec3::new(-he.x, -he.y, he.z), Vec3::new(he.x, -he.y, he.z)),
                        (Vec3::new(-he.x, -he.y, -he.z), Vec3::new(-he.x, -he.y, he.z)),
                        (Vec3::new(he.x, -he.y, -he.z), Vec3::new(he.x, -he.y, he.z)),
                        (Vec3::new(-he.x, he.y, -he.z), Vec3::new(he.x, he.y, -he.z)),
                        (Vec3::new(-he.x, he.y, he.z), Vec3::new(he.x, he.y, he.z)),
                        (Vec3::new(-he.x, he.y, -he.z), Vec3::new(-he.x, he.y, he.z)),
                        (Vec3::new(he.x, he.y, -he.z), Vec3::new(he.x, he.y, he.z)),
                        (Vec3::new(-he.x, -he.y, -he.z), Vec3::new(-he.x, he.y, -he.z)),
                        (Vec3::new(he.x, -he.y, -he.z), Vec3::new(he.x, he.y, -he.z)),
                        (Vec3::new(-he.x, -he.y, he.z), Vec3::new(-he.x, he.y, he.z)),
                        (Vec3::new(he.x, -he.y, he.z), Vec3::new(he.x, he.y, he.z)),
                    ];
                    for (a, b) in &edges {
                        let wa = w(*a);
                        let wb = w(*b);
                        let mid = (wa + wb) * 0.5;
                        out.push(SnapCandidate::edge_pt(
                            mid,
                            wa,
                            wb,
                            format!("{name} edge mid"),
                            i,
                        ));
                    }
                    for (dir, lbl) in [
                        (Vec3::X * he.x, "+X"),
                        (Vec3::NEG_X * he.x, "-X"),
                        (Vec3::Y * he.y, "+Y"),
                        (Vec3::NEG_Y * he.y, "-Y"),
                        (Vec3::Z * he.z, "+Z"),
                        (Vec3::NEG_Z * he.z, "-Z"),
                    ] {
                        let n = self.prim_feature_world_normal(i, p, dir.normalize_or_zero());
                        out.push(
                            SnapCandidate::point(
                                w(dir),
                                SnapFilter::Face,
                                format!("{name} {lbl} face"),
                                i,
                            )
                            .with_normal(n),
                        );
                    }
                }
                SdfShape::Cylinder { radius, half_height } => {
                    let r = *radius;
                    let hh = *half_height;
                    out.push(
                        SnapCandidate::point(w(Vec3::Y * hh), SnapFilter::Face, format!("{name} top"), i)
                            .with_normal(self.prim_feature_world_normal(i, p, Vec3::Y)),
                    );
                    out.push(
                        SnapCandidate::point(
                            w(Vec3::NEG_Y * hh),
                            SnapFilter::Face,
                            format!("{name} bottom"),
                            i,
                        )
                        .with_normal(self.prim_feature_world_normal(i, p, Vec3::NEG_Y)),
                    );
                    for &(dx, dz) in &[(r, 0.0f32), (-r, 0.0), (0.0, r), (0.0, -r)] {
                        out.push(SnapCandidate::point(
                            w(Vec3::new(dx, hh, dz)),
                            SnapFilter::Vertex,
                            format!("{name} rim"),
                            i,
                        ));
                        out.push(SnapCandidate::point(
                            w(Vec3::new(dx, -hh, dz)),
                            SnapFilter::Vertex,
                            format!("{name} rim"),
                            i,
                        ));
                        let a = w(Vec3::new(dx, -hh, dz));
                        let b = w(Vec3::new(dx, hh, dz));
                        out.push(SnapCandidate::edge_pt(
                            (a + b) * 0.5,
                            a,
                            b,
                            format!("{name} side mid"),
                            i,
                        ));
                    }
                }
                SdfShape::Torus { major_radius, minor_radius } => {
                    let maj = *major_radius;
                    let min = *minor_radius;
                    for &(dx, dz) in &[(maj, 0.0f32), (-maj, 0.0), (0.0, maj), (0.0, -maj)] {
                        out.push(SnapCandidate::point(
                            w(Vec3::new(dx, 0.0, dz)),
                            SnapFilter::Vertex,
                            format!("{name} major"),
                            i,
                        ));
                        let dir = Vec3::new(dx, 0.0, dz).normalize_or_zero();
                        out.push(SnapCandidate::point(
                            w(dir * maj + Vec3::Y * min),
                            SnapFilter::Vertex,
                            format!("{name} tube top"),
                            i,
                        ));
                        out.push(SnapCandidate::point(
                            w(dir * maj - Vec3::Y * min),
                            SnapFilter::Vertex,
                            format!("{name} tube bot"),
                            i,
                        ));
                    }
                }
                SdfShape::Cone { radius, half_height } => {
                    let r = *radius;
                    let hh = *half_height;
                    out.push(SnapCandidate::point(
                        w(Vec3::Y * hh),
                        SnapFilter::Vertex,
                        format!("{name} apex"),
                        i,
                    ));
                    out.push(
                        SnapCandidate::point(
                            w(Vec3::NEG_Y * hh),
                            SnapFilter::Face,
                            format!("{name} base"),
                            i,
                        )
                        .with_normal(self.prim_feature_world_normal(i, p, Vec3::NEG_Y)),
                    );
                    for &(dx, dz) in &[(r, 0.0f32), (-r, 0.0), (0.0, r), (0.0, -r)] {
                        let base = Vec3::new(dx, -hh, dz);
                        let apex = Vec3::Y * hh;
                        out.push(SnapCandidate::point(
                            w(base),
                            SnapFilter::Vertex,
                            format!("{name} base rim"),
                            i,
                        ));
                        let a = w(base);
                        let b = w(apex);
                        out.push(SnapCandidate::edge_pt(
                            (a + b) * 0.5,
                            a,
                            b,
                            format!("{name} slant mid"),
                            i,
                        ));
                    }
                }
                SdfShape::Capsule { radius, half_height } => {
                    let r = *radius;
                    let hh = *half_height;
                    out.push(SnapCandidate::point(
                        w(Vec3::Y * (hh + r)),
                        SnapFilter::Vertex,
                        format!("{name} top"),
                        i,
                    ));
                    out.push(SnapCandidate::point(
                        w(Vec3::NEG_Y * (hh + r)),
                        SnapFilter::Vertex,
                        format!("{name} bottom"),
                        i,
                    ));
                    for &(dx, dz) in &[(r, 0.0f32), (-r, 0.0), (0.0, r), (0.0, -r)] {
                        out.push(SnapCandidate::point(
                            w(Vec3::new(dx, 0.0, dz)),
                            SnapFilter::Edge,
                            format!("{name} waist"),
                            i,
                        ));
                    }
                }
                SdfShape::Stair { half_extents, .. } => {
                    let he = *half_extents;
                    for sx in [-1.0f32, 1.0] {
                        for sy in [-1.0f32, 1.0] {
                            for sz in [-1.0f32, 1.0] {
                                out.push(SnapCandidate::point(
                                    w(Vec3::new(he.x * sx, he.y * sy, he.z * sz)),
                                    SnapFilter::Vertex,
                                    format!("{name} corner"),
                                    i,
                                ));
                            }
                        }
                    }
                    for (dir, lbl) in [
                        (Vec3::X * he.x, "+X"),
                        (Vec3::NEG_X * he.x, "-X"),
                        (Vec3::Y * he.y, "+Y"),
                        (Vec3::NEG_Y * he.y, "-Y"),
                        (Vec3::Z * he.z, "+Z"),
                        (Vec3::NEG_Z * he.z, "-Z"),
                    ] {
                        let n = self.prim_feature_world_normal(i, p, dir.normalize_or_zero());
                        out.push(
                            SnapCandidate::point(
                                w(dir),
                                SnapFilter::Face,
                                format!("{name} {lbl} face"),
                                i,
                            )
                            .with_normal(n),
                        );
                    }
                }
                SdfShape::SpiralStair { radius, half_height, .. } => {
                    let r = *radius;
                    let hh = *half_height;
                    out.push(
                        SnapCandidate::point(w(Vec3::Y * hh), SnapFilter::Face, format!("{name} top"), i)
                            .with_normal(self.prim_feature_world_normal(i, p, Vec3::Y)),
                    );
                    out.push(
                        SnapCandidate::point(
                            w(Vec3::NEG_Y * hh),
                            SnapFilter::Face,
                            format!("{name} bottom"),
                            i,
                        )
                        .with_normal(self.prim_feature_world_normal(i, p, Vec3::NEG_Y)),
                    );
                    for &(dx, dz) in &[(r, 0.0f32), (-r, 0.0), (0.0, r), (0.0, -r)] {
                        out.push(SnapCandidate::point(
                            w(Vec3::new(dx, 0.0, dz)),
                            SnapFilter::Vertex,
                            format!("{name} rim"),
                            i,
                        ));
                    }
                }
                SdfShape::Billboard { width, height, .. } => {
                    let hw = *width * 0.5;
                    let hh = *height * 0.5;
                    for (sx, sy) in [(-1.0f32, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                        out.push(SnapCandidate::point(
                            w(Vec3::new(hw * sx, hh * sy, 0.0)),
                            SnapFilter::Vertex,
                            format!("{name} corner"),
                            i,
                        ));
                    }
                }
                SdfShape::CurveExtrude { profile, half_depth, .. } => {
                    let hd = *half_depth;
                    for (vi, v) in profile.verts.iter().enumerate() {
                        let local = Vec3::new(v.pos.x, 0.0, v.pos.y);
                        out.push(SnapCandidate::point(
                            w(local + Vec3::Y * hd),
                            SnapFilter::Vertex,
                            format!("{name} profile[{vi}] top"),
                            i,
                        ));
                        out.push(SnapCandidate::point(
                            w(local - Vec3::Y * hd),
                            SnapFilter::Vertex,
                            format!("{name} profile[{vi}] bot"),
                            i,
                        ));
                    }
                }
                SdfShape::MeshVolume { grid, .. } => {
                    let c = grid.corner_abs_max();
                    for sx in [-1.0f32, 1.0] {
                        for sy in [-1.0f32, 1.0] {
                            for sz in [-1.0f32, 1.0] {
                                out.push(SnapCandidate::point(
                                    w(Vec3::new(c.x * sx, c.y * sy, c.z * sz)),
                                    SnapFilter::Vertex,
                                    format!("{name} mesh corner"),
                                    i,
                                ));
                            }
                        }
                    }
                }
                SdfShape::HalfSpace => {}
                SdfShape::SpinePipe { points, .. } => {
                    for (pi, q) in points.iter().enumerate() {
                        out.push(SnapCandidate::point(
                            w(*q),
                            SnapFilter::Vertex,
                            format!("{name} spine[{pi}]"),
                            i,
                        ));
                    }
                }
            }
        }

        // World origin
        out.push(SnapCandidate::point(
            Vec3::ZERO,
            SnapFilter::Vertex,
            "World origin",
            usize::MAX,
        ));
        out.push(SnapCandidate::point(
            Vec3::ZERO,
            SnapFilter::All,
            "World origin",
            usize::MAX,
        ));

        // Selection centroid (multi-select) — WORLD positions
        {
            let mut sum = Vec3::ZERO;
            let mut n = 0u32;
            for uid in self.sel.usel.selected_prims() {
                if let Some(idx) = self.prim_index_by_uid(uid) {
                    if idx < self.scene.primitives.len() && self.scene.primitives[idx].visible {
                        sum += self.prim_feature_world(idx, &self.scene.primitives[idx], Vec3::ZERO);
                        n += 1;
                    }
                }
            }
            if n >= 2 {
                out.push(SnapCandidate::point(
                    sum / n as f32,
                    SnapFilter::All,
                    "Selection center",
                    usize::MAX,
                ));
            }
        }

        // Volume: poly / imported body AABB centers
        if self.ui2.snap_volume {
            self.append_volume_snap_candidates(&mut out);
        }

        // Poly mesh V/E/F
        self.append_poly_geometry_snap_candidates(&mut out);
        out
    }

    fn append_volume_snap_candidates(&self, out: &mut Vec<SnapCandidate>) {
        use core_math::scene_graph::NodePayload;
        for (nid, node) in self.scene.scene_graph.all_nodes() {
            if !node.visible {
                continue;
            }
            let name = {
                let n = node.payload.display_name();
                if n.is_empty() {
                    "Body"
                } else {
                    n
                }
            };
            let world = self.scene.scene_graph.world_transform(nid);
            match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => {
                    let mut bmin = Vec3::splat(f32::MAX);
                    let mut bmax = Vec3::splat(f32::MIN);
                    let mut any = false;
                    for v in &mesh.vertices {
                        if v.half_edge == core_math::half_edge::NO_EDGE {
                            continue;
                        }
                        any = true;
                        bmin = bmin.min(v.position);
                        bmax = bmax.max(v.position);
                    }
                    if any {
                        let c = world.transform_point3((bmin + bmax) * 0.5);
                        out.push(SnapCandidate::point(
                            c,
                            SnapFilter::Volume,
                            format!("{name} volume center"),
                            usize::MAX,
                        ));
                    }
                }
                NodePayload::ImportedMesh { .. }
                | NodePayload::VoxelBody { .. }
                | NodePayload::NurbsBody { .. }
                | NodePayload::NurbsSolid { .. } => {
                    // World origin of node transform as volume anchor.
                    let c = world.transform_point3(Vec3::ZERO);
                    out.push(SnapCandidate::point(
                        c,
                        SnapFilter::Volume,
                        format!("{name} volume center"),
                        usize::MAX,
                    ));
                }
                _ => {}
            }
        }
    }

    /// Emit vertex / edge-midpoint / face-center candidates from every PolygonalBody.
    /// During poly G/R/S, the moving selection is excluded so the pivot cannot snap to itself.
    pub(crate) fn append_poly_geometry_snap_candidates(&self, out: &mut Vec<SnapCandidate>) {
        use core_math::half_edge::{FaceId, HalfEdgeId, VertexId, NO_EDGE, NO_TWIN};
        use core_math::scene_graph::NodePayload;
        use std::collections::HashSet;

        let exclude_vids: HashSet<u32> = match self.tool.current.as_poly_transform() {
            Some(ts) => ts.original_positions.iter().map(|&(v, _)| v).collect(),
            None => HashSet::new(),
        };
        let exclude_nid = if exclude_vids.is_empty() {
            None
        } else {
            self.sel.usel.active_node()
        };
        let empty = HashSet::new();

        for (nid, node) in self.scene.scene_graph.all_nodes() {
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
                continue;
            };
            let excl = if exclude_nid == Some(nid) {
                &exclude_vids
            } else {
                &empty
            };
            let world = self.scene.scene_graph.world_transform(nid);
            let xf = |p: Vec3| world.transform_point3(p);
            let name = {
                let n = node.payload.display_name();
                if n.is_empty() {
                    "Poly"
                } else {
                    n
                }
            };

            // Vertices (capped — stride sample if dense)
            let mut vert_n = 0usize;
            let n_verts = mesh.vertices.len();
            let v_stride = ((n_verts / POLY_SNAP_MAX_VERTS).max(1)) as u32;
            for i in (0..n_verts as u32).step_by(v_stride as usize) {
                if excl.contains(&i) {
                    continue;
                }
                if mesh.vert(VertexId(i)).half_edge == NO_EDGE {
                    continue;
                }
                out.push(SnapCandidate::point(
                    xf(mesh.vert(VertexId(i)).position),
                    SnapFilter::Vertex,
                    format!("{name} vert"),
                    usize::MAX,
                ));
                vert_n += 1;
                if vert_n >= POLY_SNAP_MAX_VERTS {
                    break;
                }
            }

            // Undirected edge midpoints + segment for along-edge / highlight
            let mut edge_n = 0usize;
            let n_he = mesh.half_edges.len();
            let e_stride = ((n_he / (POLY_SNAP_MAX_EDGES * 2)).max(1)) as u32;
            for i in (0..n_he as u32).step_by(e_stride as usize) {
                let he = HalfEdgeId(i);
                if !mesh.is_edge_live(he) {
                    continue;
                }
                let tw = mesh.he(he).twin;
                if tw != NO_TWIN && i > tw.0 {
                    continue;
                }
                let a = mesh.he_source(he).0;
                let b = mesh.he(he).vertex.0;
                if excl.contains(&a) || excl.contains(&b) {
                    continue;
                }
                let pa = xf(mesh.vert(VertexId(a)).position);
                let pb = xf(mesh.vert(VertexId(b)).position);
                out.push(SnapCandidate::edge_pt(
                    (pa + pb) * 0.5,
                    pa,
                    pb,
                    format!("{name} edge mid"),
                    usize::MAX,
                ));
                edge_n += 1;
                if edge_n >= POLY_SNAP_MAX_EDGES {
                    break;
                }
            }

            // Face centroids
            let mut face_n = 0usize;
            let n_faces = mesh.faces.len();
            let f_stride = ((n_faces / POLY_SNAP_MAX_FACES).max(1)) as u32;
            for i in (0..n_faces as u32).step_by(f_stride as usize) {
                if mesh.face(FaceId(i)).half_edge == NO_EDGE {
                    continue;
                }
                let vs = mesh.face_vertices(FaceId(i));
                if vs.len() < 3 {
                    continue;
                }
                if vs.iter().any(|v| excl.contains(&v.0)) {
                    continue;
                }
                let mut c = Vec3::ZERO;
                for &v in &vs {
                    c += mesh.vert(v).position;
                }
                let wp = xf(c / vs.len() as f32);
                // World face normal for Align Rotation to Target (Blender parity).
                let local_n = mesh.compute_face_normal(FaceId(i)).normalize_or_zero();
                let world_n = world.transform_vector3(local_n).normalize_or_zero();
                let face = SnapCandidate::point(
                    wp,
                    SnapFilter::Face,
                    format!("{name} face center"),
                    usize::MAX,
                )
                .with_normal(world_n);
                out.push(face);
                out.push(
                    SnapCandidate::point(wp, SnapFilter::All, format!("{name} center"), usize::MAX)
                        .with_normal(world_n),
                );
                face_n += 1;
                if face_n >= POLY_SNAP_MAX_FACES {
                    break;
                }
            }
        }
    }

}
