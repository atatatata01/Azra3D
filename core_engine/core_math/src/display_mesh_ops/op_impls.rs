use glam::Vec3;

use super::{
    build_vertex_adjacency,
    DecimateMode, DecimateParams, DisplayMesh,
};
use super::normals::mesh_recompute_normals;

// ============================================================
// Kernels
// ============================================================

/// Mirror: for each enabled axis, duplicate mesh reflected across the plane through `center`.
/// Multiple axes compose (X then Y then Z reflections of the growing set — 2^n copies of original).
pub(super) fn mesh_mirror(
    mesh: DisplayMesh,
    axis_bits: u8,
    center: Vec3,
    weld: bool,
    weld_threshold: f32,
) -> DisplayMesh {
    if axis_bits == 0 || mesh.positions.is_empty() {
        return mesh;
    }

    // Collect reflection combos (same bit order as field true-union: enabled axes only).
    let n_axes = (axis_bits & 7).count_ones();
    let n_combo = 1u32 << n_axes;
    if n_combo <= 1 {
        return mesh;
    }

    let base_v = mesh.positions.len();
    let mut positions = Vec::with_capacity(base_v * n_combo as usize);
    let mut normals = Vec::with_capacity(base_v * n_combo as usize);
    let mut indices = Vec::with_capacity(mesh.indices.len() * n_combo as usize);

    for combo in 0..n_combo {
        let mut sign = [1.0f32; 3];
        let mut bit = 0u32;
        for axis in 0..3 {
            if (axis_bits >> axis) & 1 != 0 {
                if (combo >> bit) & 1 != 0 {
                    sign[axis] = -1.0;
                }
                bit += 1;
            }
        }
        let odd_reflections = sign.iter().filter(|&&s| s < 0.0).count() % 2 == 1;
        let v_off = positions.len() as u32;

        for i in 0..base_v {
            let p = mesh.positions[i];
            let n = mesh.normals.get(i).copied().unwrap_or(Vec3::Y);
            let mut rp = p;
            let mut rn = n;
            for axis in 0..3 {
                if sign[axis] < 0.0 {
                    rp[axis] = 2.0 * center[axis] - p[axis];
                    rn[axis] = -n[axis];
                }
            }
            positions.push(rp);
            normals.push(rn.normalize_or_zero());
        }

        // Indices: flip winding on odd reflection count so normals face outward.
        let tri_count = mesh.indices.len() / 3;
        for t in 0..tri_count {
            let i0 = mesh.indices[t * 3];
            let i1 = mesh.indices[t * 3 + 1];
            let i2 = mesh.indices[t * 3 + 2];
            if odd_reflections {
                indices.push(v_off + i0);
                indices.push(v_off + i2);
                indices.push(v_off + i1);
            } else {
                indices.push(v_off + i0);
                indices.push(v_off + i1);
                indices.push(v_off + i2);
            }
        }
    }

    let mut out = DisplayMesh {
        positions,
        normals,
        indices,
    };
    if weld {
        out = weld_by_distance(out, weld_threshold.max(1e-8));
    }
    out
}

/// Weld verts within `thr` using a **spatial hash** (O(n) expected), not O(n²) pairwise scan.
pub(super) fn weld_by_distance(mesh: DisplayMesh, thr: f32) -> DisplayMesh {
    use std::collections::HashMap;

    let n = mesh.positions.len();
    if n == 0 {
        return mesh;
    }
    let thr = thr.max(1e-12);
    let thr2 = thr * thr;
    let inv = 1.0 / thr;

    // cell → list of output vertex indices already placed in that cell
    let mut grid: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::with_capacity(n);
    let mut remap = vec![0u32; n];
    let mut new_pos: Vec<Vec3> = Vec::with_capacity(n);
    let mut new_nrm: Vec<Vec3> = Vec::with_capacity(n);

    let cell_of = |p: Vec3| -> (i32, i32, i32) {
        (
            (p.x * inv).floor() as i32,
            (p.y * inv).floor() as i32,
            (p.z * inv).floor() as i32,
        )
    };

    for i in 0..n {
        let p = mesh.positions[i];
        let nrm_i = mesh.normals.get(i).copied().unwrap_or(Vec3::Y);
        let c = cell_of(p);
        let mut found: Option<u32> = None;
        // Search this cell and 26 neighbors (full 3×3×3)
        'search: for dx in -1i32..=1 {
            for dy in -1i32..=1 {
                for dz in -1i32..=1 {
                    let key = (c.0 + dx, c.1 + dy, c.2 + dz);
                    if let Some(list) = grid.get(&key) {
                        for &j in list {
                            if (new_pos[j as usize] - p).length_squared() <= thr2 {
                                found = Some(j);
                                break 'search;
                            }
                        }
                    }
                }
            }
        }
        if let Some(j) = found {
            remap[i] = j;
            new_nrm[j as usize] += nrm_i;
        } else {
            let j = new_pos.len() as u32;
            remap[i] = j;
            new_pos.push(p);
            new_nrm.push(nrm_i);
            grid.entry(c).or_default().push(j);
        }
    }
    for nrm in &mut new_nrm {
        *nrm = nrm.normalize_or_zero();
        if nrm.length_squared() < 1e-12 {
            *nrm = Vec3::Y;
        }
    }
    let mut clean = Vec::with_capacity(mesh.indices.len());
    for t in mesh.indices.chunks_exact(3) {
        let a = remap[t[0] as usize];
        let b = remap[t[1] as usize];
        let c = remap[t[2] as usize];
        if a != b && b != c && a != c {
            clean.extend_from_slice(&[a, b, c]);
        }
    }
    DisplayMesh {
        positions: new_pos,
        normals: new_nrm,
        indices: clean,
    }
}


pub(super) fn mesh_smooth_laplacian(mut mesh: DisplayMesh, iterations: u32, factor: f32) -> DisplayMesh {
    let factor = factor.clamp(0.0, 1.0);
    let n = mesh.positions.len();
    if n == 0 || iterations == 0 || factor <= 0.0 {
        return mesh;
    }

    let adj = build_vertex_adjacency(n, &mesh.indices);

    for _ in 0..iterations {
        let prev = mesh.positions.clone();
        for i in 0..n {
            let nbrs = &adj[i];
            if nbrs.is_empty() {
                continue;
            }
            let mut avg = Vec3::ZERO;
            for &j in nbrs {
                avg += prev[j as usize];
            }
            avg /= nbrs.len() as f32;
            mesh.positions[i] = prev[i].lerp(avg, factor);
        }
    }
    // normals stay; caller may append SmoothNormals
    mesh
}

/// Decimate toward `params.ratio` of original triangle count.
///
/// **Principle #1 (always):** collapse flats first, retain curvature.
///
/// | Mode | Kernel | Industry analogue |
/// |------|--------|-------------------|
/// | Master | QEM + vertex importance map + flat-first | ZBrush Decimation Master |
/// | Quality | QEM + flat-first dihedral | Blender Collapse |
/// | Planar | Collapse only near-coplanar edges | Blender Planar |
/// | Fast | length / flatness^p | interactive |
/// | Field | Quality + SDF \|H\| assist | experimental |
///
/// Blind spatial cluster is never used — it destroys adaptive density.
pub(super) fn mesh_decimate(
    mesh: DisplayMesh,
    params: DecimateParams,
    field: Option<&crate::field_eval::BodyField>,
) -> DisplayMesh {
    let params = params.clamped();
    let n_tri = mesh.triangle_count();
    if n_tri < 4 || params.ratio >= 0.999 {
        return mesh;
    }
    let target_tri = ((n_tri as f32) * params.ratio).ceil().max(2.0) as usize;

    // Huge DC meshes: pre-pass Fast flat-first down to a safe size, then quality kernel.
    // Prevents UI freeze/crash when adding Decimate on 100k+ tri bodies.
    let mut mesh = mesh;
    const SAFE_QEM_TRIS: usize = 60_000;
    let wants_qem = matches!(
        params.mode,
        DecimateMode::Quality | DecimateMode::Master | DecimateMode::Field
    );
    if wants_qem && n_tri > SAFE_QEM_TRIS {
        let pre_target = target_tri.max(SAFE_QEM_TRIS);
        if n_tri > pre_target {
            mesh = mesh_decimate_qem(
                mesh,
                pre_target,
                None,
                None,
                params,
                DecimateKernelOpts {
                    use_qem: false,
                    use_importance: false,
                    planar_only: false,
                },
            );
        }
    }
    if mesh.triangle_count() <= target_tri {
        return mesh;
    }

    match params.mode {
        DecimateMode::Fast => mesh_decimate_qem(
            mesh,
            target_tri,
            None,
            None,
            params,
            DecimateKernelOpts {
                use_qem: false,
                use_importance: false,
                planar_only: false,
            },
        ),
        DecimateMode::Quality => mesh_decimate_qem(
            mesh,
            target_tri,
            None,
            None,
            params,
            DecimateKernelOpts {
                use_qem: true,
                use_importance: false,
                planar_only: false,
            },
        ),
        DecimateMode::Planar => mesh_decimate_qem(
            mesh,
            target_tri,
            None,
            None,
            params,
            DecimateKernelOpts {
                use_qem: false,
                use_importance: false,
                planar_only: true,
            },
        ),
        DecimateMode::Master => {
            // Decimation Master family: QEM + mesh importance.
            // Optional light SDF |H| assist (field_strength) — never full edge Dijkstra.
            let curv = if params.field_strength > 1e-6 {
                field.map(|f| sample_sdf_vertex_curvature(f, &mesh, params.field_radius))
            } else {
                None
            };
            let target_len = if params.use_edge_length_field {
                Some(build_target_edge_length(
                    &mesh,
                    curv.as_deref(),
                    params.min_edge_len,
                    params.soft_feature_world,
                    params.curve_angle_deg,
                ))
            } else {
                None
            };
            mesh_decimate_qem(
                mesh,
                target_tri,
                curv.as_deref(),
                target_len.as_deref(),
                params,
                DecimateKernelOpts {
                    use_qem: true,
                    use_importance: true,
                    planar_only: false,
                },
            )
        }
        DecimateMode::Field => {
            let mut curv = if params.field_strength > 1e-6 {
                field.map(|f| sample_sdf_vertex_curvature(f, &mesh, params.field_radius))
            } else {
                None
            };
            // Optional full AZRA edge-shape (ridge + distance-to-feature). Costly; prefer
            // Master-first quality folds that only use dual-scale |H|.
            if params.edge_shape_strength > 1e-6 {
                if let Some(f) = field {
                    let edge = sample_sdf_edge_shape_map(
                        f,
                        &mesh,
                        params.field_radius,
                        params.soft_feature_world,
                    );
                    let w = params.edge_shape_strength.clamp(0.0, 4.0);
                    if let Some(ref mut c) = curv {
                        let n = c.len().min(edge.len());
                        for i in 0..n {
                            c[i] = c[i].max(edge[i] * (0.55 + 0.45 * w));
                        }
                    } else {
                        curv = Some(
                            edge
                                .into_iter()
                                .map(|e| e * (0.55 + 0.45 * w))
                                .collect(),
                        );
                    }
                }
            }
            let target_len = if params.use_edge_length_field {
                Some(build_target_edge_length(
                    &mesh,
                    curv.as_deref(),
                    params.min_edge_len,
                    params.soft_feature_world,
                    params.curve_angle_deg,
                ))
            } else {
                None
            };
            mesh_decimate_qem(
                mesh,
                target_tri,
                curv.as_deref(),
                target_len.as_deref(),
                params,
                DecimateKernelOpts {
                    use_qem: true,
                    use_importance: true,
                    planar_only: false,
                },
            )
        }
    }
}


/// Per-vertex detail importance (Decimation Master style).
/// 0 = all incident faces coplanar; 1 = high normal variance (detail/curve).
fn build_vertex_importance(n_vert: usize, indices: &[u32], face_n: &[Vec3]) -> Vec<f32> {
    let mut acc = vec![Vec3::ZERO; n_vert];
    let mut cnt = vec![0u32; n_vert];
    // Also track max pairwise face-normal disagreement (sharper feature signal).
    let mut max_dev = vec![0.0f32; n_vert];
    let mut face_list: Vec<Vec<u32>> = vec![Vec::new(); n_vert];
    for (fi, t) in indices.chunks_exact(3).enumerate() {
        let n = face_n.get(fi).copied().unwrap_or(Vec3::Y);
        for &vi in t {
            let i = vi as usize;
            if i < n_vert {
                acc[i] += n;
                cnt[i] += 1;
                face_list[i].push(fi as u32);
            }
        }
    }
    for i in 0..n_vert {
        let faces = &face_list[i];
        if faces.len() < 2 {
            continue;
        }
        let mut md = 0.0f32;
        for a in 0..faces.len() {
            let na = face_n.get(faces[a] as usize).copied().unwrap_or(Vec3::Y);
            for b in (a + 1)..faces.len() {
                let nb = face_n.get(faces[b] as usize).copied().unwrap_or(Vec3::Y);
                let d = (1.0 - na.dot(nb).clamp(-1.0, 1.0)) * 0.5;
                if d > md {
                    md = d;
                }
            }
        }
        max_dev[i] = md;
    }
    let mut out = vec![0.0f32; n_vert];
    for i in 0..n_vert {
        if cnt[i] == 0 {
            continue;
        }
        // Mean of unit face normals; |mean|≈1 on flats, smaller on curved regions.
        let mean_len = (acc[i] / cnt[i] as f32).length().clamp(0.0, 1.0);
        let variance = (1.0 - mean_len).clamp(0.0, 1.0);
        // Blend variance + max dihedral disagreement (Decimation Master–like detail map).
        let detail = (variance * 0.65 + max_dev[i] * 0.55).clamp(0.0, 1.0);
        // Mild gamma: lift mid-detail so soft fillets compete with hard edges.
        out[i] = detail.powf(0.82);
    }
    out
}

#[derive(Clone, Copy)]
struct DecimateKernelOpts {
    use_qem: bool,
    /// Decimation Master–style per-vertex keep weight from face-normal variance.
    use_importance: bool,
    /// Blender Planar: only collapse edges flatter than crease_angle_deg.
    planar_only: bool,
}

/// Finite-difference mean-curvature proxy \|H\| at each mesh vertex from the body SDF.
///
/// Uses the same 6-neighbor Laplacian as body field Smooth (`lap * eps²` dimensionless).
/// High values mark creases / peaks / cavities that Decimate should protect.
/// `radius_scale` multiplies average edge length for the FD step (see `DecimateParams::field_radius`).
///
/// **Dual-scale:** samples at fine (`eps`) and coarse (`~2.4× eps`) and takes the max so
/// thin mesh edges and soft-K fillet bands both register as protect signal.
pub fn sample_sdf_vertex_curvature(
    field: &crate::field_eval::BodyField,
    mesh: &DisplayMesh,
    radius_scale: f32,
) -> Vec<f32> {
    let n = mesh.positions.len();
    if n == 0 {
        return Vec::new();
    }
    // Feature scale from average edge length (surface-local, not bbox).
    let sample_n = mesh.triangle_count().min(1_500).max(1);
    let mut edge_sum = 0.0f32;
    let mut edge_c = 0u32;
    for t in mesh.indices.chunks_exact(3).take(sample_n) {
        let p0 = mesh.positions[t[0] as usize];
        let p1 = mesh.positions[t[1] as usize];
        let p2 = mesh.positions[t[2] as usize];
        edge_sum += (p0 - p1).length() + (p1 - p2).length() + (p2 - p0).length();
        edge_c += 3;
    }
    let avg_edge = (edge_sum / edge_c.max(1) as f32).max(1e-5);
    let scale = radius_scale.clamp(0.15, 3.0);
    let eps_fine = (avg_edge * scale).clamp(1e-4, avg_edge * 4.0);
    // Coarse pass catches soft-wall thickness (~soft-K / bevel) that fine mesh-edge FD misses.
    let eps_coarse = (eps_fine * 2.4).clamp(eps_fine * 1.15, avg_edge * 8.0);

    let sample_h = |p: Vec3, eps: f32, d0: f32| -> f32 {
        let dxp = field.eval_or(p + Vec3::X * eps, d0);
        let dxm = field.eval_or(p - Vec3::X * eps, d0);
        let dyp = field.eval_or(p + Vec3::Y * eps, d0);
        let dym = field.eval_or(p - Vec3::Y * eps, d0);
        let dzp = field.eval_or(p + Vec3::Z * eps, d0);
        let dzm = field.eval_or(p - Vec3::Z * eps, d0);
        // Dimensionless |laplacian * eps²| ≈ |sum_n - 6d|.
        (dxp + dxm + dyp + dym + dzp + dzm - 6.0 * d0).abs()
    };

    let mut out = vec![0.0f32; n];
    for (i, &p) in mesh.positions.iter().enumerate() {
        let d = field.eval_or(p, 0.0);
        // Only near-surface verts drive collapse policy (use coarse band).
        if d.abs() > eps_coarse * 4.0 {
            continue;
        }
        let h_fine = sample_h(p, eps_fine, d);
        let h_coarse = sample_h(p, eps_coarse, d);
        // Mild boost on coarse so soft fillets compete with sharp mesh dihedrals.
        out[i] = h_fine.max(h_coarse * 1.15).clamp(0.0, 4.0);
    }
    out
}

/// Per-vertex mesh importance 0..1 (face-normal variance) for debug heat maps / UI.
/// 0 = locally planar; 1 = high detail / curve.
pub fn mesh_vertex_importance(mesh: &DisplayMesh) -> Vec<f32> {
    let n = mesh.positions.len();
    if n == 0 || mesh.indices.len() < 3 {
        return Vec::new();
    }
    let mut face_n = Vec::with_capacity(mesh.triangle_count());
    for t in mesh.indices.chunks_exact(3) {
        let p0 = mesh.positions[t[0] as usize];
        let p1 = mesh.positions[t[1] as usize];
        let p2 = mesh.positions[t[2] as usize];
        let nrm = (p1 - p0).cross(p2 - p0);
        face_n.push(if nrm.length_squared() > 1e-20 {
            nrm.normalize()
        } else {
            Vec3::Y
        });
    }
    build_vertex_importance(n, &mesh.indices, &face_n)
}

/// Average edge length of a display mesh (sample-capped). Used for soft-feature radius.
pub fn mesh_avg_edge_length(mesh: &DisplayMesh) -> f32 {
    let sample_n = mesh.triangle_count().min(1_500).max(1);
    let mut edge_sum = 0.0f32;
    let mut edge_c = 0u32;
    for t in mesh.indices.chunks_exact(3).take(sample_n) {
        let p0 = mesh.positions[t[0] as usize];
        let p1 = mesh.positions[t[1] as usize];
        let p2 = mesh.positions[t[2] as usize];
        edge_sum += (p0 - p1).length() + (p1 - p2).length() + (p2 - p0).length();
        edge_c += 3;
    }
    (edge_sum / edge_c.max(1) as f32).max(1e-5)
}

/// Per-vertex **target edge length** ℓ(x) (world units).
///
/// High protect (ridge / soft-K / crease) → short ℓ (floor at `min_edge`).
/// Flats → long ℓ so QEM melts interiors and keeps edge bands even.
/// `protect_src` is optional Field/edge-map assist in ~0..4 units (same as curv).
pub fn build_target_edge_length(
    mesh: &DisplayMesh,
    protect_src: Option<&[f32]>,
    min_edge: f32,
    soft_feature: f32,
    curve_angle_deg: f32,
) -> Vec<f32> {
    let n = mesh.positions.len();
    if n == 0 {
        return Vec::new();
    }
    let avg = mesh_avg_edge_length(mesh);
    let floor = if min_edge > 1e-8 {
        min_edge
    } else {
        avg * 1.25
    };
    // Flat target: several× min so interiors dump tris hard.
    let flat_max = (floor * 4.5).max(avg * 3.0);
    // Curve ring target from arc: R·θ, or min edge.
    let ang = if curve_angle_deg > 0.5 {
        curve_angle_deg
    } else {
        12.0
    };
    let curve_tgt = if soft_feature > 1e-5 {
        (soft_feature * ang.to_radians()).max(floor * 0.85)
    } else {
        floor
    };
    let mesh_imp = mesh_vertex_importance(mesh);
    let mut out = vec![flat_max; n];
    for i in 0..n {
        let mut p = mesh_imp.get(i).copied().unwrap_or(0.0);
        if let Some(src) = protect_src {
            p = p.max(src.get(i).copied().unwrap_or(0.0) * 0.25);
        }
        p = p.clamp(0.0, 1.0);
        // Smoothstep protect so mid-band transitions are gentle.
        let t = p * p * (3.0 - 2.0 * p);
        // High protect → curve_tgt / floor; low → flat_max.
        let dense = curve_tgt.min(floor * 1.15).max(floor * 0.9);
        out[i] = flat_max * (1.0 - t) + dense * t;
    }
    out
}

/// Laplacian on **flat / low-protect** verts only, then project back to the SDF surface.
///
/// **Analytical feature project** — straighten hard edges & flatten planes after LO.
///
/// 1. Detect **crease edges** (dihedral > threshold).
/// 2. Build **connected crease chains** and **fit a 3D line** (PCA) to each chain ≥3 verts.
/// 3. Project chain verts onto that line (full snap, clamped).
/// 4. Project **flat** verts (low importance) onto local mean plane.
/// 5. Optional **SDF surface pull** so verts sit on the field isosurface.
///
/// This is much more visible than per-triangle plane–plane (noisy after decimate).
pub fn mesh_analytical_feature_project(
    mut mesh: DisplayMesh,
    field: Option<&crate::field_eval::BodyField>,
    max_move: f32,
    crease_angle_deg: f32,
) -> DisplayMesh {
    let n = mesh.positions.len();
    if n == 0 || mesh.indices.len() < 3 {
        return mesh;
    }
    let max_move = max_move.max(1e-6);
    let crease_cos = crease_angle_deg.to_radians().cos().clamp(-1.0, 1.0);

    // Face unit normals.
    let n_tri = mesh.triangle_count();
    let mut face_n = Vec::with_capacity(n_tri);
    for t in mesh.indices.chunks_exact(3) {
        let p0 = mesh.positions[t[0] as usize];
        let p1 = mesh.positions[t[1] as usize];
        let p2 = mesh.positions[t[2] as usize];
        let nn = (p1 - p0).cross(p2 - p0);
        face_n.push(if nn.length_squared() > 1e-20 {
            nn.normalize()
        } else {
            Vec3::Y
        });
    }

    // Edge key → up to 2 face indices (manifold).
    let mut edge_faces: std::collections::HashMap<(u32, u32), [i32; 2]> =
        std::collections::HashMap::with_capacity(n_tri * 2);
    for (fi, t) in mesh.indices.chunks_exact(3).enumerate() {
        for &(a, b) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a < b { (a, b) } else { (b, a) };
            let e = edge_faces.entry(key).or_insert([-1, -1]);
            if e[0] < 0 {
                e[0] = fi as i32;
            } else if e[1] < 0 {
                e[1] = fi as i32;
            }
        }
    }

    // Crease edges: dihedral sharper than threshold.
    let mut crease_edges: Vec<(u32, u32)> = Vec::new();
    for (&(a, b), faces) in &edge_faces {
        if faces[0] < 0 || faces[1] < 0 {
            continue;
        }
        let n0 = face_n[faces[0] as usize];
        let n1 = face_n[faces[1] as usize];
        let c = n0.dot(n1).clamp(-1.0, 1.0);
        // sharper than crease_angle → crease (e.g. 90° box edge: c≈0)
        if c < crease_cos {
            crease_edges.push((a, b));
        }
    }

    // Build adjacency for crease graph.
    let mut cadj: Vec<Vec<u32>> = vec![Vec::new(); n];
    for &(a, b) in &crease_edges {
        cadj[a as usize].push(b);
        cadj[b as usize].push(a);
    }
    for list in &mut cadj {
        list.sort_unstable();
        list.dedup();
    }

    // Connected components → chains; fit line and project.
    let mut visited = vec![false; n];
    let mut new_pos = mesh.positions.clone();
    let mut on_crease = vec![false; n];

    for start in 0..n {
        if visited[start] || cadj[start].is_empty() {
            continue;
        }
        // Collect component.
        let mut stack = vec![start];
        let mut comp = Vec::new();
        visited[start] = true;
        while let Some(u) = stack.pop() {
            comp.push(u);
            for &v in &cadj[u] {
                let vu = v as usize;
                if !visited[vu] {
                    visited[vu] = true;
                    stack.push(vu);
                }
            }
        }
        if comp.len() < 2 {
            continue;
        }
        for &u in &comp {
            on_crease[u] = true;
        }
        // Fit line through component (PCA / inertia).
        let (origin, dir) = fit_line_pca(&mesh.positions, &comp);
        if dir.length_squared() < 1e-12 {
            continue;
        }
        let dir = dir.normalize();
        for &u in &comp {
            let p = mesh.positions[u];
            let t = (p - origin).dot(dir);
            let on_line = origin + dir * t;
            let delta = on_line - p;
            let len = delta.length();
            // Full snap for crease chains (clamped) — this is the visible straighten.
            let p_new = if len > max_move {
                p + delta * (max_move / len)
            } else {
                on_line
            };
            if p_new.is_finite() {
                new_pos[u] = p_new;
            }
        }
    }

    // Flat project: verts not on crease, low importance → local plane.
    let imp = mesh_vertex_importance(&mesh);
    let mut vfaces: Vec<Vec<u32>> = vec![Vec::new(); n];
    for (fi, t) in mesh.indices.chunks_exact(3).enumerate() {
        for &vi in t {
            let i = vi as usize;
            if i < n {
                vfaces[i].push(fi as u32);
            }
        }
    }
    for i in 0..n {
        if on_crease[i] {
            continue;
        }
        let detail = imp.get(i).copied().unwrap_or(0.0);
        if detail > 0.28 {
            continue; // mid/high detail — leave alone (soft-K etc.)
        }
        let faces = &vfaces[i];
        if faces.is_empty() {
            continue;
        }
        let mut n_avg = Vec3::ZERO;
        let mut p0 = mesh.positions[i];
        for &fi in faces {
            n_avg += face_n[fi as usize];
        }
        n_avg = n_avg.normalize_or_zero();
        if n_avg.length_squared() < 1e-12 {
            continue;
        }
        // Plane through mean of incident face centroids.
        let mut csum = Vec3::ZERO;
        let mut cc = 0u32;
        for &fi in faces {
            let base = fi as usize * 3;
            let ia = mesh.indices[base] as usize;
            let ib = mesh.indices[base + 1] as usize;
            let ic = mesh.indices[base + 2] as usize;
            csum += (mesh.positions[ia] + mesh.positions[ib] + mesh.positions[ic]) / 3.0;
            cc += 1;
        }
        if cc > 0 {
            p0 = csum / cc as f32;
        }
        let p = new_pos[i];
        let d = n_avg.dot(p - p0);
        let on_plane = p - n_avg * d;
        let w = (1.0 - detail * 2.5).clamp(0.5, 1.0);
        let mut p_new = p.lerp(on_plane, w);
        let delta = p_new - p;
        let len = delta.length();
        if len > max_move {
            p_new = p + delta * (max_move / len);
        }
        if p_new.is_finite() {
            new_pos[i] = p_new;
        }
    }

    // SDF surface pull for everyone (tight to field) — stronger than before.
    if let Some(f) = field {
        const EPS: f32 = 0.0012;
        let k = [
            Vec3::new(1.0, -1.0, -1.0),
            Vec3::new(-1.0, -1.0, 1.0),
            Vec3::new(-1.0, 1.0, -1.0),
            Vec3::new(1.0, 1.0, 1.0),
        ];
        for i in 0..n {
            let mut p = new_pos[i];
            let p0 = p;
            for _ in 0..3 {
                let d = f.eval_or(p, 0.0);
                if !d.is_finite() || d.abs() > max_move * 3.0 {
                    break;
                }
                let g = k[0] * f.eval_or(p + k[0] * EPS, d)
                    + k[1] * f.eval_or(p + k[1] * EPS, d)
                    + k[2] * f.eval_or(p + k[2] * EPS, d)
                    + k[3] * f.eval_or(p + k[3] * EPS, d);
                if g.length_squared() < 1e-16 {
                    break;
                }
                // Crease verts: lighter pull so line project holds; flats: stronger.
                let pull = if on_crease[i] { 0.55 } else { 0.9 };
                p -= g.normalize() * (d * pull);
            }
            let delta = p - p0;
            let len = delta.length();
            let p_new = if len > max_move {
                p0 + delta * (max_move / len)
            } else {
                p
            };
            if p_new.is_finite() {
                new_pos[i] = p_new;
            }
        }
    }

    mesh.positions = new_pos;
    mesh_drop_degenerate_tris(&mesh.positions, &mut mesh.indices);
    mesh_orient_faces_consistent(&mesh.positions, &mut mesh.indices);
    mesh_recompute_normals(mesh)
}

/// Fit a 3D line to positions[comp] via centroid + largest principal axis (covariance).
fn fit_line_pca(positions: &[Vec3], comp: &[usize]) -> (Vec3, Vec3) {
    let mut c = Vec3::ZERO;
    for &i in comp {
        c += positions[i];
    }
    c /= comp.len().max(1) as f32;
    // Covariance 3×3
    let mut xx = 0.0f32;
    let mut yy = 0.0f32;
    let mut zz = 0.0f32;
    let mut xy = 0.0f32;
    let mut xz = 0.0f32;
    let mut yz = 0.0f32;
    for &i in comp {
        let d = positions[i] - c;
        xx += d.x * d.x;
        yy += d.y * d.y;
        zz += d.z * d.z;
        xy += d.x * d.y;
        xz += d.x * d.z;
        yz += d.y * d.z;
    }
    // Power iteration for dominant eigenvector of cov.
    let mut v = Vec3::new(1.0, 0.1, 0.05);
    // Prefer longest axis of AABB as seed (more stable).
    let mut bmin = Vec3::splat(f32::MAX);
    let mut bmax = Vec3::splat(f32::MIN);
    for &i in comp {
        bmin = bmin.min(positions[i]);
        bmax = bmax.max(positions[i]);
    }
    let ext = bmax - bmin;
    if ext.x >= ext.y && ext.x >= ext.z {
        v = Vec3::X;
    } else if ext.y >= ext.z {
        v = Vec3::Y;
    } else {
        v = Vec3::Z;
    }
    for _ in 0..12 {
        let nv = Vec3::new(
            xx * v.x + xy * v.y + xz * v.z,
            xy * v.x + yy * v.y + yz * v.z,
            xz * v.x + yz * v.y + zz * v.z,
        );
        if nv.length_squared() < 1e-20 {
            break;
        }
        v = nv.normalize();
    }
    (c, v)
}

/// Kills star fans on large walls without melting soft-K or hard edges. Pure field snap —
/// no analytical rims.
pub fn mesh_flat_relax_sdf_snap(
    mut mesh: DisplayMesh,
    field: &crate::field_eval::BodyField,
    protect: Option<&[f32]>,
    iterations: u32,
    factor: f32,
    snap_steps: u32,
) -> DisplayMesh {
    let n = mesh.positions.len();
    if n == 0 || iterations == 0 {
        return mesh;
    }
    let factor = factor.clamp(0.05, 0.75);
    let adj = build_vertex_adjacency(n, &mesh.indices);
    // Protect 0..1: high = ridge (don't move).
    let prot: Vec<f32> = if let Some(p) = protect {
        (0..n)
            .map(|i| (p.get(i).copied().unwrap_or(0.0) * 0.25).clamp(0.0, 1.0))
            .collect()
    } else {
        let imp = mesh_vertex_importance(&mesh);
        imp.into_iter()
            .map(|v| v.clamp(0.0, 1.0))
            .collect()
    };

    // Precompute face normals for flip checks.
    let face_n0 = {
        let mut fnm = Vec::with_capacity(mesh.triangle_count());
        for t in mesh.indices.chunks_exact(3) {
            let p0 = mesh.positions[t[0] as usize];
            let p1 = mesh.positions[t[1] as usize];
            let p2 = mesh.positions[t[2] as usize];
            fnm.push((p1 - p0).cross(p2 - p0));
        }
        fnm
    };
    // Vert -> incident faces.
    let mut vfaces: Vec<Vec<u32>> = vec![Vec::new(); n];
    for (fi, t) in mesh.indices.chunks_exact(3).enumerate() {
        for &vi in t {
            if (vi as usize) < n {
                vfaces[vi as usize].push(fi as u32);
            }
        }
    }

    let faces_flip = |positions: &[Vec3], vi: usize, old_fn: &[Vec3]| -> bool {
        for &fi in &vfaces[vi] {
            let base = fi as usize * 3;
            if base + 2 >= mesh.indices.len() {
                continue;
            }
            let i0 = mesh.indices[base] as usize;
            let i1 = mesh.indices[base + 1] as usize;
            let i2 = mesh.indices[base + 2] as usize;
            let nn = (positions[i1] - positions[i0]).cross(positions[i2] - positions[i0]);
            let on = old_fn.get(fi as usize).copied().unwrap_or(Vec3::ZERO);
            if on.length_squared() > 1e-20 && nn.length_squared() > 1e-20 {
                if on.dot(nn) < 0.0 {
                    return true;
                }
            }
            if nn.length_squared() < 1e-20 {
                return true;
            }
        }
        false
    };

    for _ in 0..iterations {
        let prev = mesh.positions.clone();
        let nrm = {
            let mut tmp = mesh.clone();
            tmp.positions = prev.clone();
            mesh_recompute_normals(tmp).normals
        };
        for i in 0..n {
            if prot[i] > 0.38 {
                continue;
            }
            let nbrs = &adj[i];
            if nbrs.is_empty() {
                continue;
            }
            let mut avg = Vec3::ZERO;
            for &j in nbrs {
                avg += prev[j as usize];
            }
            avg /= nbrs.len() as f32;
            let mut delta = (avg - prev[i]) * factor;
            let n_i = nrm.get(i).copied().unwrap_or(Vec3::Y);
            delta -= n_i * delta.dot(n_i);
            let w = 1.0 - prot[i];
            let candidate = prev[i] + delta * w;
            // Trial: only accept if no incident face flips.
            let mut trial = prev.clone();
            trial[i] = candidate;
            if !faces_flip(&trial, i, &face_n0) {
                mesh.positions[i] = candidate;
            }
        }
        // SDF project — also flip-guarded.
        let steps = snap_steps.max(1);
        const EPS: f32 = 0.0015;
        let k = [
            Vec3::new(1.0, -1.0, -1.0),
            Vec3::new(-1.0, -1.0, 1.0),
            Vec3::new(-1.0, 1.0, -1.0),
            Vec3::new(1.0, 1.0, 1.0),
        ];
        for i in 0..n {
            let mut p = mesh.positions[i];
            let p_before = p;
            for _ in 0..steps {
                let d = field.eval_or(p, 0.0);
                if !d.is_finite() {
                    break;
                }
                let g = k[0] * field.eval_or(p + k[0] * EPS, d)
                    + k[1] * field.eval_or(p + k[1] * EPS, d)
                    + k[2] * field.eval_or(p + k[2] * EPS, d)
                    + k[3] * field.eval_or(p + k[3] * EPS, d);
                if g.length_squared() < 1e-16 {
                    break;
                }
                let w = if prot[i] > 0.38 { 0.35 } else { 1.0 };
                p -= g.normalize() * (d * w * 0.85); // slightly damped
            }
            if p.is_finite() {
                let mut trial = mesh.positions.clone();
                trial[i] = p;
                if !faces_flip(&trial, i, &face_n0) {
                    mesh.positions[i] = p;
                } else {
                    mesh.positions[i] = p_before;
                }
            }
        }
    }
    // Final orientation fix after relax.
    mesh_orient_faces_consistent(&mesh.positions, &mut mesh.indices);
    mesh_recompute_normals(mesh)
}

/// **AZRA-only edge-shape map** — per-vertex protect strength in the same ~0..4 units as
/// [`sample_sdf_vertex_curvature`].
///
/// Combines:
/// 1. **SDF ridge / anisotropy** — second-derivative variance (hard edges + soft-K walls)
///    dual-scale so thin mesh edges and fillet thickness both register.
/// 2. **Graph distance-to-feature** — multi-source Dijkstra from ridge seeds; falloff band
///    from soft-feature world size so density concentrates *around* the edge shape.
/// 3. **Mesh dihedral importance** — boosts hard creases the SDF FD may under-sample.
///
/// Pure field + mesh topology. No analytical rims. Flats score near 0 → feed `flat_starve`.
pub fn sample_sdf_edge_shape_map(
    field: &crate::field_eval::BodyField,
    mesh: &DisplayMesh,
    radius_scale: f32,
    soft_feature_world: f32,
) -> Vec<f32> {
    let n = mesh.positions.len();
    if n == 0 {
        return Vec::new();
    }
    let avg_edge = mesh_avg_edge_length(mesh);
    let scale = radius_scale.clamp(0.15, 3.0);
    let eps_fine = (avg_edge * scale).clamp(1e-4, avg_edge * 4.0);
    let eps_coarse = (eps_fine * 2.4).clamp(eps_fine * 1.15, avg_edge * 8.0);

    // --- 1) Dual-scale anisotropic curvature (ridge) + mean |H| ---
    let mut ridge = vec![0.0f32; n];
    let mut h_mean = vec![0.0f32; n];
    for (i, &p) in mesh.positions.iter().enumerate() {
        let d = field.eval_or(p, 0.0);
        if d.abs() > eps_coarse * 4.0 {
            continue;
        }
        let (h_f, a_f) = sdf_h_and_aniso(field, p, d, eps_fine);
        let (h_c, a_c) = sdf_h_and_aniso(field, p, d, eps_coarse);
        h_mean[i] = h_f.max(h_c * 1.1);
        // Anisotropy catches creases / soft walls; mean H catches isotropic soft blobs.
        ridge[i] = a_f.max(a_c * 1.15).max(h_mean[i] * 0.55);
    }

    // Normalize ridge by robust high percentile so values sit in 0..1 before scaling.
    let mut sorted: Vec<f32> = ridge.iter().copied().filter(|&v| v > 1e-6).collect();
    let p95 = if sorted.is_empty() {
        1.0
    } else {
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((sorted.len() as f32 * 0.95) as usize).min(sorted.len() - 1);
        sorted[idx].max(1e-4)
    };
    for v in &mut ridge {
        *v = (*v / p95).clamp(0.0, 1.0);
    }

    // --- 2) Mesh dihedral importance boost (hard edges DC already resolves) ---
    let mesh_imp = mesh_vertex_importance(mesh);
    for i in 0..n {
        if let Some(&mi) = mesh_imp.get(i) {
            ridge[i] = ridge[i].max(mi * 0.85);
        }
    }

    // --- 3) Graph distance-to-feature (more mesh near edge shape) ---
    let band = if soft_feature_world > 1e-5 {
        soft_feature_world.max(avg_edge * 1.5)
    } else {
        avg_edge * 3.0
    };
    let prox = mesh_feature_proximity(mesh, &ridge, band, 0.42);

    // Final protect: ridge peak OR proximity falloff (band around the edge).
    let mut out = vec![0.0f32; n];
    for i in 0..n {
        let p = ridge[i].max(prox[i]);
        // Map to Field assist units (~0..4).
        out[i] = (p * 4.0).clamp(0.0, 4.0);
    }
    out
}

/// Dimensionless mean |H| and anisotropy of second derivatives at `p`.
fn sdf_h_and_aniso(
    field: &crate::field_eval::BodyField,
    p: Vec3,
    d: f32,
    eps: f32,
) -> (f32, f32) {
    let dxp = field.eval_or(p + Vec3::X * eps, d);
    let dxm = field.eval_or(p - Vec3::X * eps, d);
    let dyp = field.eval_or(p + Vec3::Y * eps, d);
    let dym = field.eval_or(p - Vec3::Y * eps, d);
    let dzp = field.eval_or(p + Vec3::Z * eps, d);
    let dzm = field.eval_or(p - Vec3::Z * eps, d);
    // s_ii ≈ ∂²f/∂x_i² * eps² (dimensionless).
    let sxx = dxp + dxm - 2.0 * d;
    let syy = dyp + dym - 2.0 * d;
    let szz = dzp + dzm - 2.0 * d;
    let h = (sxx + syy + szz).abs();
    let mean = (sxx + syy + szz) / 3.0;
    let var = ((sxx - mean) * (sxx - mean)
        + (syy - mean) * (syy - mean)
        + (szz - mean) * (szz - mean))
        / 3.0;
    let aniso = var.max(0.0).sqrt();
    (h.clamp(0.0, 4.0), aniso.clamp(0.0, 4.0))
}

/// Multi-source Dijkstra on mesh edges: proximity 1 at ridge seeds → exp(-dist/band).
///
/// Seeds = verts with ridge ≥ `seed_thresh`. Band is world length of the density halo
/// around main edges (soft-K thickness or ~few avg edges).
fn mesh_feature_proximity(
    mesh: &DisplayMesh,
    ridge: &[f32],
    band: f32,
    seed_thresh: f32,
) -> Vec<f32> {
    use std::cmp::Ordering;
    use std::collections::BinaryHeap;

    let n = mesh.positions.len();
    if n == 0 || ridge.len() < n {
        return vec![0.0; n];
    }
    let band = band.max(1e-5);

    // Undirected adjacency with lengths.
    let mut adj: Vec<Vec<(u32, f32)>> = vec![Vec::new(); n];
    {
        let mut seen = std::collections::HashSet::new();
        for t in mesh.indices.chunks_exact(3) {
            for &(a, b) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                let key = if a < b { (a, b) } else { (b, a) };
                if !seen.insert(key) {
                    continue;
                }
                let pa = mesh.positions[a as usize];
                let pb = mesh.positions[b as usize];
                let len = (pa - pb).length().max(1e-6);
                adj[a as usize].push((b, len));
                adj[b as usize].push((a, len));
            }
        }
    }

    #[derive(Clone, Copy)]
    struct Node {
        d: f32,
        i: u32,
    }
    impl PartialEq for Node {
        fn eq(&self, o: &Self) -> bool {
            self.d == o.d && self.i == o.i
        }
    }
    impl Eq for Node {}
    impl PartialOrd for Node {
        fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
            Some(self.cmp(o))
        }
    }
    impl Ord for Node {
        fn cmp(&self, o: &Self) -> Ordering {
            // min-heap on distance
            o.d.partial_cmp(&self.d)
                .unwrap_or(Ordering::Equal)
                .then(self.i.cmp(&o.i))
        }
    }

    let mut dist = vec![f32::INFINITY; n];
    let mut heap = BinaryHeap::new();
    for i in 0..n {
        if ridge[i] >= seed_thresh {
            dist[i] = 0.0;
            heap.push(Node { d: 0.0, i: i as u32 });
        }
    }
    if heap.is_empty() {
        // No strong seeds — use top ridge verts as soft seeds.
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| ridge[b].partial_cmp(&ridge[a]).unwrap_or(Ordering::Equal));
        let take = (n / 40).clamp(8, 256).min(n);
        for &i in order.iter().take(take) {
            if ridge[i] > 1e-4 {
                dist[i] = 0.0;
                heap.push(Node { d: 0.0, i: i as u32 });
            }
        }
    }

    while let Some(Node { d, i }) = heap.pop() {
        let iu = i as usize;
        if d > dist[iu] + 1e-9 {
            continue;
        }
        // Cap search radius (~3 bands) for cost.
        if d > band * 3.0 {
            continue;
        }
        for &(j, w) in &adj[iu] {
            let nd = d + w;
            let ju = j as usize;
            if nd < dist[ju] {
                dist[ju] = nd;
                heap.push(Node { d: nd, i: j });
            }
        }
    }

    let mut out = vec![0.0f32; n];
    for i in 0..n {
        if !dist[i].is_finite() {
            continue;
        }
        out[i] = (-dist[i] / band).exp().clamp(0.0, 1.0);
    }
    out
}

// ------------------------------------------------------------
// QEM (Garland–Heckbert) + optional SDF curvature weights
// ------------------------------------------------------------

/// Symmetric 4×4 quadric stored as 10 unique upper-triangle coeffs.
#[derive(Clone, Copy, Default)]
struct Quadric {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
    g: f32,
    h: f32,
    i: f32,
    j: f32,
}

impl Quadric {
    fn from_plane(n: Vec3, p: Vec3) -> Self {
        let nn = n.length_squared();
        if nn < 1e-16 {
            return Self::default();
        }
        let n = n * nn.sqrt().recip();
        let w = -n.dot(p);
        // K = vvᵀ for v = [nx, ny, nz, w]
        Self {
            a: n.x * n.x,
            b: n.x * n.y,
            c: n.x * n.z,
            d: n.x * w,
            e: n.y * n.y,
            f: n.y * n.z,
            g: n.y * w,
            h: n.z * n.z,
            i: n.z * w,
            j: w * w,
        }
    }

    fn add(self, o: Self) -> Self {
        Self {
            a: self.a + o.a,
            b: self.b + o.b,
            c: self.c + o.c,
            d: self.d + o.d,
            e: self.e + o.e,
            f: self.f + o.f,
            g: self.g + o.g,
            h: self.h + o.h,
            i: self.i + o.i,
            j: self.j + o.j,
        }
    }

    /// Scale plane contribution (area-weighted QEM — large faces matter more).
    fn scale(self, s: f32) -> Self {
        Self {
            a: self.a * s,
            b: self.b * s,
            c: self.c * s,
            d: self.d * s,
            e: self.e * s,
            f: self.f * s,
            g: self.g * s,
            h: self.h * s,
            i: self.i * s,
            j: self.j * s,
        }
    }

    fn error(self, p: Vec3) -> f32 {
        let (x, y, z) = (p.x, p.y, p.z);
        // vᵀ Q v with v = [x,y,z,1]
        self.a * x * x
            + 2.0 * self.b * x * y
            + 2.0 * self.c * x * z
            + 2.0 * self.d * x
            + self.e * y * y
            + 2.0 * self.f * y * z
            + 2.0 * self.g * y
            + self.h * z * z
            + 2.0 * self.i * z
            + self.j
    }

    /// Optimal placement; falls back to midpoint if the 3×3 is singular.
    fn optimal(self, fallback: Vec3) -> Vec3 {
        // Solve A p = -b where A is top-left 3×3 of Q, b = (d,g,i).
        let det = self.a * (self.e * self.h - self.f * self.f)
            - self.b * (self.b * self.h - self.f * self.c)
            + self.c * (self.b * self.f - self.e * self.c);
        if det.abs() < 1e-10 {
            return fallback;
        }
        let inv = 1.0 / det;
        // Cramer's rule for A p = (-d, -g, -i)
        let bx = -self.d;
        let by = -self.g;
        let bz = -self.i;
        let x = inv
            * (bx * (self.e * self.h - self.f * self.f)
                - self.b * (by * self.h - self.f * bz)
                + self.c * (by * self.f - self.e * bz));
        let y = inv
            * (self.a * (by * self.h - self.f * bz)
                - bx * (self.b * self.h - self.f * self.c)
                + self.c * (self.b * bz - by * self.c));
        let z = inv
            * (self.a * (self.e * bz - by * self.f)
                - self.b * (self.b * bz - by * self.c)
                + bx * (self.b * self.f - self.e * self.c));
        let p = Vec3::new(x, y, z);
        if p.is_finite() {
            p
        } else {
            fallback
        }
    }
}

/// Incremental QEM edge-collapse (Garland–Heckbert), **flat first, curvature last**.
///
/// O(n log n): a vertex→incident-face adjacency (`vf`) is maintained so every collapse
/// touches ONLY the faces around the merged pair — flip-check, reindex, normal recompute
/// and heap re-injection are all O(local degree). There is NO periodic full rebuild (that
/// was the legacy O(n²/48) "too slow / unusable" path). Lazy heap: stale entries are
/// dropped by a per-vertex generation counter + an `alive` flag (no find/union needed —
/// faces are reindexed eagerly). Non-manifold is allowed (this is a DISPLAY mesh, never
/// fed to topology ops); only fold-overs are rejected (normal-flip guard).
///
/// `opts.use_qem`: true → quadric error; false → length base (Fast). `curv[v]` = optional
/// SDF |H| assist in [0,4] (assist only — never replaces the mesh dihedral signal).
/// `target_len[v]` = optional desired edge length (world) from AZRA edge-length field.
fn mesh_decimate_qem(
    mesh: DisplayMesh,
    target_tri: usize,
    curv: Option<&[f32]>,
    target_len: Option<&[f32]>,
    params: DecimateParams,
    opts: DecimateKernelOpts,
) -> DisplayMesh {
    use std::collections::BinaryHeap;
    let params = params.clamped();
    let nv = mesh.positions.len();
    let mut pos = mesh.positions;
    let mut faces: Vec<[u32; 3]> = mesh.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
    let nf0 = faces.len();
    if nf0 <= target_tri || nv < 4 {
        let mut out = DisplayMesh { positions: pos, normals: mesh.normals, indices: mesh.indices };
        if out.normals.len() != out.positions.len() { out = mesh_recompute_normals(out); }
        return out;
    }

    let unit_or = |n: Vec3| -> Vec3 { if n.length_squared() > 1e-20 { n.normalize() } else { Vec3::Y } };
    let tri_n = |f: &[u32; 3], pos: &[Vec3]| -> Vec3 {
        (pos[f[1] as usize] - pos[f[0] as usize]).cross(pos[f[2] as usize] - pos[f[0] as usize])
    };

    let mut face_alive = vec![true; nf0];
    let mut fnorm: Vec<Vec3> = faces.iter().map(|f| unit_or(tri_n(f, &pos))).collect();

    // Per-vertex quadrics + incident-face adjacency (both maintained through collapses).
    // Area-weighted plane contributions: large flats dominate error → melt first correctly.
    let mut q = vec![Quadric::default(); nv];
    let mut vf: Vec<Vec<u32>> = vec![Vec::new(); nv];
    let mut face_area = vec![0.0f32; nf0];
    let mut area_mean = 0.0f32;
    for (fi, f) in faces.iter().enumerate() {
        let cross = tri_n(f, &pos);
        let area = cross.length() * 0.5;
        face_area[fi] = area.max(1e-12);
        area_mean += face_area[fi];
        // Weight ∝ area so big coplanar regions collapse before tiny detail faces.
        let pq = Quadric::from_plane(fnorm[fi], pos[f[0] as usize]).scale(face_area[fi]);
        for &v in f {
            q[v as usize] = q[v as usize].add(pq);
            vf[v as usize].push(fi as u32);
        }
    }
    area_mean = (area_mean / nf0.max(1) as f32).max(1e-12);

    // Decimation-Master importance (variance + max dihedral disagreement).
    let importance: Vec<f32> = if opts.use_importance {
        let flat_idx: Vec<u32> = faces.iter().flat_map(|f| f.iter().copied()).collect();
        build_vertex_importance(nv, &flat_idx, &fnorm)
    } else {
        Vec::new()
    };

    let crease_cos = params.crease_angle_deg.to_radians().cos().clamp(-1.0, 1.0);
    let crease_strength = params.crease_strength;
    let boundary_mult = if params.boundary_strength > 1e-6 { 1.0 + 7.0 * params.boundary_strength } else { 1.0 };
    let field_boost = 2.0 * params.field_strength;
    let flat_power = (2.0 + 3.0 * params.planar_bias).clamp(2.0, 14.0);
    let max_error = params.max_error;
    let use_qem = opts.use_qem;
    let use_importance = opts.use_importance;
    let planar_only = opts.planar_only;
    let area_mean_inv = 1.0 / area_mean;
    let edge_shape = params.edge_shape_strength.clamp(0.0, 4.0);
    let flat_starve = params.flat_starve.clamp(0.0, 4.0);
    let min_edge = params.min_edge_len.max(0.0);
    let curve_angle = params.curve_angle_deg.clamp(0.0, 45.0);
    // Soft-feature / min-edge used for arc chord budget on curved ridges.
    let soft_feat = params.soft_feature_world.max(0.0);

    // Chain turning angle (degrees) at vertex `v` looking toward `other`.
    // Finds the best opposite neighbor and measures bend from a straight chain.
    // 0° = colinear (straight edge); higher = curve sample / kink.
    let chain_turn_deg = |v: u32,
                          other: u32,
                          pos: &[Vec3],
                          vf: &[Vec<u32>],
                          faces: &[[u32; 3]],
                          face_alive: &[bool]|
     -> f32 {
        let pv = pos[v as usize];
        let d0 = pos[other as usize] - pv;
        let l0 = d0.length();
        if l0 < 1e-12 {
            return 0.0;
        }
        let d0n = d0 / l0;
        let mut best_colinear = -2.0f32;
        let mut best_turn = 180.0f32;
        for &fi in &vf[v as usize] {
            let fu = fi as usize;
            if !face_alive[fu] {
                continue;
            }
            for &w in &faces[fu] {
                if w == v || w == other {
                    continue;
                }
                let dw = pos[w as usize] - pv;
                let lw = dw.length();
                if lw < 1e-12 {
                    continue;
                }
                let dwn = dw / lw;
                // Colinear continuation: direction toward `other` vs opposite of toward `w`.
                // cos≈1 → straight chain other—v—w.
                let cos_col = (-d0n.dot(dwn)).clamp(-1.0, 1.0);
                if cos_col > best_colinear {
                    best_colinear = cos_col;
                    best_turn = cos_col.acos().to_degrees();
                }
            }
        }
        if best_colinear < -1.5 {
            // Dead-end / corner with no opposite neighbor — treat as free for short collapse.
            return 0.0;
        }
        best_turn
    };

    // Cost of collapsing edge (a,b). Takes the live buffers as args (no capture → free to
    // call while they are being mutated). Dihedral comes from the ≤2 faces shared by a & b,
    // found by scanning vf[a] — O(degree). Returns (weighted cost, raw QEM error, placement).
    let edge_cost = |a: u32, b: u32, pos: &[Vec3], q: &[Quadric], vf: &[Vec<u32>], faces: &[[u32; 3]], fnorm: &[Vec3], face_alive: &[bool], face_area: &[f32]| -> (f32, f32, Vec3) {
        let pa = pos[a as usize];
        let pb = pos[b as usize];
        let mid = (pa + pb) * 0.5;
        let len2 = (pa - pb).length_squared().max(1e-20);
        let edge_len = len2.sqrt();
        let (raw, p) = if use_qem {
            let qa = q[a as usize].add(q[b as usize]);
            let popt = qa.optimal(mid);
            let t = (popt - pa).dot(pb - pa) / len2;
            let p = if (0.0..=1.0).contains(&t) { popt } else { mid };
            (qa.error(p).max(0.0), p)
        } else { (len2, mid) };
        let base = if use_qem { raw + 1e-10 * len2 + 1e-16 } else { len2 };
        // Shared faces (dihedral) — scan the smaller adjacency.
        let (sa, sb) = if vf[a as usize].len() <= vf[b as usize].len() { (a, b) } else { (b, a) };
        let mut nfound = 0u32;
        let (mut n0, mut n1) = (Vec3::ZERO, Vec3::ZERO);
        let mut shared_area = 0.0f32;
        for &fi in &vf[sa as usize] {
            let fu = fi as usize;
            if !face_alive[fu] { continue; }
            let f = &faces[fu];
            if f[0] == sb || f[1] == sb || f[2] == sb {
                shared_area += face_area.get(fu).copied().unwrap_or(area_mean);
                if nfound == 0 { n0 = fnorm[fu]; nfound = 1; }
                else { n1 = fnorm[fu]; nfound = 2; break; }
            }
        }
        let (flatness, curve, cos_n, is_boundary) = if nfound >= 2 {
            let c = n0.dot(n1).clamp(-1.0, 1.0);
            (c.max(0.0), ((1.0 - c) * 0.5).clamp(0.0, 1.0), c, false)
        } else if nfound == 1 { (0.25, 0.5, 0.0, true) } else { (0.5, 0.5, 0.0, false) };

        let flat_denom = flatness.powf(flat_power) + 1e-5;
        let mut curve_mul = 1.0 + curve * curve * (10.0 + 50.0 * crease_strength);
        if !is_boundary && cos_n < crease_cos && crease_strength > 1e-6 {
            let tt = ((crease_cos - cos_n) / (1.0 + crease_cos).max(1e-3)).clamp(0.0, 1.0);
            curve_mul *= 1.0 + tt * (30.0 + 80.0 * crease_strength);
        }
        let mut imp_ab = 0.0f32;
        if use_importance && !importance.is_empty() {
            imp_ab = importance.get(a as usize).copied().unwrap_or(0.0)
                .max(importance.get(b as usize).copied().unwrap_or(0.0));
            curve_mul *= 1.0 + imp_ab * imp_ab * (14.0 + 45.0 * crease_strength);
        }
        let ha = curv.and_then(|c| c.get(a as usize).copied()).unwrap_or(0.0);
        let hb = curv.and_then(|c| c.get(b as usize).copied()).unwrap_or(0.0);
        let hmax = ha.max(hb);
        let hmin = ha.min(hb);
        if field_boost > 1e-6 && hmax > 1e-4 {
            curve_mul *= 1.0 + hmax * field_boost;
        }

        // Chain turn at both endpoints (0 = straight, high = round / kink).
        let use_angle = curve_angle > 0.5 || min_edge > 1e-8;
        let (turn_a, turn_b, max_turn) = if use_angle {
            let ta = chain_turn_deg(a, b, pos, vf, faces, face_alive);
            let tb = chain_turn_deg(b, a, pos, vf, faces, face_alive);
            (ta, tb, ta.max(tb))
        } else {
            (0.0, 0.0, 0.0)
        };
        let angle_lim = if curve_angle > 0.5 { curve_angle } else { 12.0 };
        // Short + colinear: zipper on straight edge → collapse freely (reduce tris, keep shape).
        let short_colinear = min_edge > 1e-8
            && edge_len < min_edge
            && max_turn < angle_lim
            && !is_boundary
            && cos_n > crease_cos.min(0.85); // not a hard crease kink

        // AZRA edge-shape: keep main edge chains (straight or soft-curved).
        // Skip heavy protect on short colinear zippers — min-edge owns those.
        if edge_shape > 1e-6 && hmax > 1e-4 && !short_colinear {
            let p = (hmax * 0.25).clamp(0.0, 1.0);
            curve_mul *= 1.0 + edge_shape * p * p * (18.0 + 40.0 * edge_shape);
            if hmax > 0.35 && hmin < hmax * 0.35 {
                // Collapsing feature vert into flat neighbor — destroy straightness.
                curve_mul *= 1.0 + edge_shape * 5.5;
            }
            // Both on feature with real turn → keep curve samples (not straight run).
            if hmin > 0.45 && max_turn >= angle_lim * 0.65 {
                curve_mul *= 1.0 + edge_shape * 2.2;
            }
        }
        // Curve angle budget: protect vertices where the chain turns (round rings / fillets).
        if curve_angle > 0.5 && max_turn >= angle_lim && hmax > 0.2 {
            let over = ((max_turn / angle_lim) - 1.0).clamp(0.0, 3.0);
            let w = if edge_shape > 1e-6 { edge_shape } else { 1.0 };
            curve_mul *= 1.0 + w * (2.5 + 4.0 * over) * (hmax * 0.25).clamp(0.15, 1.0);
        }
        // Arc chord cap: on curved ridges, don't leave edges longer than ~R·θ.
        if curve_angle > 0.5 && hmax > 0.45 {
            let r_proxy = if soft_feat > 1e-5 {
                soft_feat
            } else if min_edge > 1e-8 {
                min_edge * 2.0
            } else {
                edge_len
            };
            let max_chord = (r_proxy * angle_lim.to_radians()).max(min_edge * 0.5);
            if edge_len > max_chord * 1.05 {
                // Already a long curve segment — don't collapse neighbors into it freely;
                // protect this edge so rings keep even spacing.
                curve_mul *= 1.0 + 2.5 * ((edge_len / max_chord) - 1.0).min(2.0);
            }
        }

        if planar_only && !is_boundary && cos_n < crease_cos {
            return (f32::MAX * 0.25, raw, p);
        }
        let mut cost = base * curve_mul / flat_denom;
        // Large flat faces: melt faster (cheaper collapse). Curved regions ignore this.
        if flatness > 0.92 && shared_area > 0.0 {
            let rel = (shared_area * area_mean_inv).clamp(0.25, 8.0);
            cost /= 1.0 + 0.55 * (rel - 0.25).max(0.0) * flatness;
        }
        // Flat starve: dump budget from planar / low-feature interiors into edge bands.
        if flat_starve > 1e-6 {
            let protect = (hmax * 0.25).max(imp_ab).max(curve).clamp(0.0, 1.0);
            let flat_s = (1.0 - protect) * flatness.clamp(0.0, 1.0);
            if flat_s > 0.15 {
                cost /= 1.0 + flat_starve * flat_s * flat_s * (12.0 + 20.0 * flat_starve);
            }
        }
        // Min edge: short colinear edges collapse first (reduce tri count, keep edge shape).
        if short_colinear {
            let t = (edge_len / min_edge).clamp(0.02, 1.0);
            cost *= t * t * 0.12;
        } else if min_edge > 1e-8 && edge_len < min_edge * 0.65 && max_turn < angle_lim * 1.25 {
            // Mild bias even without perfect flatness.
            let t = (edge_len / min_edge).clamp(0.05, 1.0);
            cost *= 0.35 + 0.65 * t;
        }
        // Target edge-length field ℓ(x): collapse when shorter than local target;
        // protect when already near target on features (even density, fewer zippers).
        if let Some(tl) = target_len {
            let la = tl.get(a as usize).copied().unwrap_or(0.0);
            let lb = tl.get(b as usize).copied().unwrap_or(0.0);
            let l_tgt = la.min(lb);
            if l_tgt > 1e-8 {
                let ratio_e = edge_len / l_tgt;
                if ratio_e < 0.85 {
                    // Too short for this region → cheap.
                    let t = ratio_e.clamp(0.05, 0.85);
                    cost *= 0.15 + 0.55 * t * t;
                } else if ratio_e > 1.2 && hmax > 0.25 {
                    // Near target on a feature — don't thin further.
                    cost *= 1.0 + 2.0 * ((ratio_e - 1.2).min(1.5));
                }
            }
        }
        // Silence unused turn endpoints if only max_turn used in some paths.
        let _ = (turn_a, turn_b);

        if is_boundary && boundary_mult > 1.0 { cost *= boundary_mult; }
        if !cost.is_finite() || cost < 0.0 { cost = f32::MAX * 0.25; }
        (cost, raw, p)
    };

    #[derive(Clone, Copy)]
    struct Cand { cost: f32, a: u32, b: u32, ga: u32, gb: u32 }
    impl PartialEq for Cand { fn eq(&self, o: &Self) -> bool { self.cost == o.cost } }
    impl Eq for Cand {}
    impl PartialOrd for Cand { fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(o)) } }
    impl Ord for Cand {
        fn cmp(&self, o: &Self) -> std::cmp::Ordering {
            // min-heap on cost
            o.cost.partial_cmp(&self.cost).unwrap_or(std::cmp::Ordering::Equal)
                .then(self.a.cmp(&o.a)).then(self.b.cmp(&o.b))
        }
    }

    let mut alive = vec![true; nv];
    let mut gen = vec![0u32; nv];
    let mut heap: BinaryHeap<Cand> = BinaryHeap::new();
    {
        let mut seen = std::collections::HashSet::new();
        for f in &faces {
            for &(x, y) in &[(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
                let key = if x < y { (x, y) } else { (y, x) };
                if seen.insert(key) {
                    let (c, _, _) = edge_cost(key.0, key.1, &pos, &q, &vf, &faces, &fnorm, &face_alive, &face_area);
                    heap.push(Cand { cost: c, a: key.0, b: key.1, ga: gen[key.0 as usize], gb: gen[key.1 as usize] });
                }
            }
        }
    }

    let mut live_tris = nf0;
    let mut guard = 0usize;
    let guard_max = nf0.saturating_mul(6) + 256;

    while live_tris > target_tri {
        guard += 1;
        if guard > guard_max { break; }
        let Some(cand) = heap.pop() else { break };
        let (a, b) = (cand.a, cand.b);
        if a == b || !alive[a as usize] || !alive[b as usize] { continue; }
        if gen[a as usize] != cand.ga || gen[b as usize] != cand.gb { continue; }
        let (cost, raw, p_new) = edge_cost(a, b, &pos, &q, &vf, &faces, &fnorm, &face_alive, &face_area);
        if !p_new.is_finite() { continue; }
        if cost >= f32::MAX * 0.2 { if planar_only { break; } continue; }
        if max_error > 0.0 && raw > max_error { continue; }

        // Fold-over + degeneracy guard: every LIVE face incident to a OR b.
        // Strict: reject inverted, near-inverted, or collapsed-area faces (mesh stays tight).
        let mut flip = false;
        'fg: for &side in &[a, b] {
            for &fi in &vf[side as usize] {
                let fu = fi as usize;
                if !face_alive[fu] { continue; }
                let mut v = faces[fu];
                for x in &mut v { if *x == b { *x = a; } }
                if v[0] == v[1] || v[1] == v[2] || v[0] == v[2] { continue; } // face dies
                let gp = |vv: u32| if vv == a { p_new } else { pos[vv as usize] };
                let e1 = gp(v[1]) - gp(v[0]);
                let e2 = gp(v[2]) - gp(v[0]);
                let nn = e1.cross(e2);
                let new_area2 = nn.length_squared();
                // Degenerate / zero-area after collapse → reject (prevents cracks & random winding).
                let old_area2 = {
                    let o1 = pos[faces[fu][1] as usize] - pos[faces[fu][0] as usize];
                    let o2 = pos[faces[fu][2] as usize] - pos[faces[fu][0] as usize];
                    o1.cross(o2).length_squared()
                };
                if new_area2 < 1e-20 || (old_area2 > 1e-18 && new_area2 < old_area2 * 1e-4) {
                    flip = true;
                    break 'fg;
                }
                let old_n = fnorm[fu];
                if old_n.length_squared() > 1e-20 {
                    // Unit-aware: reject any face that reverses or goes nearly edge-on.
                    let cos = old_n.dot(nn) / (old_n.length() * new_area2.sqrt());
                    if cos < 0.12 {
                        flip = true;
                        break 'fg;
                    }
                }
            }
        }
        if flip { continue; }

        // --- commit: b → a at p_new ---
        pos[a as usize] = p_new;
        q[a as usize] = q[a as usize].add(q[b as usize]);
        alive[b as usize] = false;
        let bfaces = std::mem::take(&mut vf[b as usize]);
        for fi in bfaces {
            let fu = fi as usize;
            if !face_alive[fu] { continue; }
            for x in faces[fu].iter_mut() { if *x == b { *x = a; } }
            let f = faces[fu];
            if f[0] == f[1] || f[1] == f[2] || f[0] == f[2] {
                face_alive[fu] = false;
                live_tris -= 1;
            } else {
                vf[a as usize].push(fi);
                // Refresh face area after collapse (for melt bias).
                let cross = tri_n(&faces[fu], &pos);
                face_area[fu] = (cross.length() * 0.5).max(1e-12);
            }
        }
        vf[a as usize].retain(|&fi| face_alive[fi as usize]);
        vf[a as usize].sort_unstable();
        vf[a as usize].dedup();
        for &fi in &vf[a as usize] {
            fnorm[fi as usize] = unit_or(tri_n(&faces[fi as usize], &pos));
            let cross = tri_n(&faces[fi as usize], &pos);
            face_area[fi as usize] = (cross.length() * 0.5).max(1e-12);
        }
        gen[a as usize] = gen[a as usize].wrapping_add(1);

        // Re-inject the (few) edges around a with fresh costs.
        let mut nbrs: Vec<u32> = Vec::with_capacity(vf[a as usize].len() * 2);
        for &fi in &vf[a as usize] {
            for &v in &faces[fi as usize] {
                if v != a && alive[v as usize] { nbrs.push(v); }
            }
        }
        nbrs.sort_unstable();
        nbrs.dedup();
        for v in nbrs {
            let (lo, hi) = if a < v { (a, v) } else { (v, a) };
            let (c, _, _) = edge_cost(lo, hi, &pos, &q, &vf, &faces, &fnorm, &face_alive, &face_area);
            heap.push(Cand { cost: c, a: lo, b: hi, ga: gen[lo as usize], gb: gen[hi as usize] });
        }
    }

    // --- rebuild the final mesh from live faces + compact verts ---
    let mut idx: Vec<u32> = Vec::with_capacity(live_tris * 3);
    for (fi, f) in faces.iter().enumerate() {
        if face_alive[fi] && f[0] != f[1] && f[1] != f[2] && f[0] != f[2] {
            idx.extend_from_slice(&[f[0], f[1], f[2]]);
        }
    }
    let mut used = vec![false; nv];
    for &i in &idx { used[i as usize] = true; }
    let mut remap = vec![u32::MAX; nv];
    let mut new_pos = Vec::new();
    for i in 0..nv {
        if used[i] { remap[i] = new_pos.len() as u32; new_pos.push(pos[i]); }
    }
    let mut final_idx: Vec<u32> = idx.iter().map(|&i| remap[i as usize]).collect();
    // Drop residual degenerates + orient consistently (fixes flipped polygons after fold).
    mesh_drop_degenerate_tris(&new_pos, &mut final_idx);
    mesh_orient_faces_consistent(&new_pos, &mut final_idx);
    mesh_recompute_normals(DisplayMesh {
        positions: new_pos,
        normals: Vec::new(),
        indices: final_idx,
    })
}

/// Remove zero-area / needle triangles that survive collapse.
fn mesh_drop_degenerate_tris(pos: &[Vec3], indices: &mut Vec<u32>) {
    let mut out = Vec::with_capacity(indices.len());
    for t in indices.chunks_exact(3) {
        let i0 = t[0] as usize;
        let i1 = t[1] as usize;
        let i2 = t[2] as usize;
        if i0 >= pos.len() || i1 >= pos.len() || i2 >= pos.len() {
            continue;
        }
        if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
            continue;
        }
        let e1 = pos[i1] - pos[i0];
        let e2 = pos[i2] - pos[i0];
        let a2 = e1.cross(e2).length_squared();
        // Relative to edge lengths — reject needles.
        let el = (e1.length() + e2.length() + (pos[i2] - pos[i1]).length()).max(1e-12);
        if a2 < 1e-20 || a2 < (el * el) * 1e-10 {
            continue;
        }
        out.extend_from_slice(t);
    }
    *indices = out;
}

/// Propagate face orientation from a seed so adjacent faces share consistent winding.
/// Fixes flipped polygons that slipped past per-collapse guards.
fn mesh_orient_faces_consistent(pos: &[Vec3], indices: &mut [u32]) {
    let n_tri = indices.len() / 3;
    if n_tri == 0 {
        return;
    }
    // Undirected edge → faces (rebuild after each flip would be expensive; we re-read winding live).
    let mut edge_faces: std::collections::HashMap<(u32, u32), Vec<u32>> =
        std::collections::HashMap::with_capacity(n_tri * 2);
    for fi in 0..n_tri {
        let i = fi * 3;
        let verts = [indices[i], indices[i + 1], indices[i + 2]];
        for e in 0..3 {
            let a = verts[e];
            let b = verts[(e + 1) % 3];
            let key = if a < b { (a, b) } else { (b, a) };
            edge_faces.entry(key).or_default().push(fi as u32);
        }
    }
    let directed = |indices: &[u32], fi: usize, u: u32, v: u32| -> bool {
        let i = fi * 3;
        let verts = [indices[i], indices[i + 1], indices[i + 2]];
        for e in 0..3 {
            if verts[e] == u && verts[(e + 1) % 3] == v {
                return true;
            }
        }
        false
    };
    let mut visited = vec![false; n_tri];
    let mut queue = std::collections::VecDeque::new();
    for seed in 0..n_tri {
        if visited[seed] {
            continue;
        }
        visited[seed] = true;
        queue.push_back(seed);
        while let Some(fi) = queue.pop_front() {
            let i = fi * 3;
            let verts = [indices[i], indices[i + 1], indices[i + 2]];
            for e in 0..3 {
                let a = verts[e];
                let b = verts[(e + 1) % 3];
                let key = if a < b { (a, b) } else { (b, a) };
                let Some(adj) = edge_faces.get(&key) else {
                    continue;
                };
                for &fj_u in adj {
                    let fj = fj_u as usize;
                    if fj == fi || visited[fj] {
                        continue;
                    }
                    // Neighbor must use opposite directed edge. If it also has a→b, flip it.
                    if directed(indices, fj, a, b) {
                        let j = fj * 3;
                        indices.swap(j + 1, j + 2);
                    }
                    visited[fj] = true;
                    queue.push_back(fj);
                }
            }
        }
    }
    // Geometric safety: any face opposing the sum of neighbor normals gets flipped.
    let mut face_n = Vec::with_capacity(n_tri);
    for t in indices.chunks_exact(3) {
        let p0 = pos[t[0] as usize];
        let p1 = pos[t[1] as usize];
        let p2 = pos[t[2] as usize];
        face_n.push((p1 - p0).cross(p2 - p0));
    }
    for fi in 0..n_tri {
        let i = fi * 3;
        let verts = [indices[i], indices[i + 1], indices[i + 2]];
        let mut acc = Vec3::ZERO;
        let mut cnt = 0u32;
        for e in 0..3 {
            let a = verts[e];
            let b = verts[(e + 1) % 3];
            let key = if a < b { (a, b) } else { (b, a) };
            if let Some(adj) = edge_faces.get(&key) {
                for &fj_u in adj {
                    let fj = fj_u as usize;
                    if fj != fi {
                        acc += face_n[fj];
                        cnt += 1;
                    }
                }
            }
        }
        if cnt > 0
            && face_n[fi].length_squared() > 1e-20
            && acc.length_squared() > 1e-20
            && face_n[fi].dot(acc) < 0.0
        {
            indices.swap(i + 1, i + 2);
            face_n[fi] = -face_n[fi];
        }
    }
}

/// LEGACY adaptive edge collapse. Correct output, but its periodic full heap+edge-map
/// rebuild (every ~48 collapses) is O(n²/48) → "too slow / unusable" on large DC meshes.
/// Superseded by the incremental `mesh_decimate_qem` above (O(n log n), local adjacency).
/// Kept only for A/B reference; delete once the incremental path is confirmed on hardware.
#[allow(dead_code)]
fn mesh_decimate_qem_legacy(
    mesh: DisplayMesh,
    target_tri: usize,
    curv: Option<&[f32]>,
    params: DecimateParams,
    opts: DecimateKernelOpts,
) -> DisplayMesh {
    use std::cmp::Ordering;
    use std::collections::{BinaryHeap, HashMap};

    let use_qem = opts.use_qem;
    let use_importance = opts.use_importance;
    let planar_only = opts.planar_only;
    let params = params.clamped();
    let n_tri = mesh.triangle_count();
    if n_tri <= target_tri {
        return mesh;
    }

    // NO uniform cluster pre-pass: it equalizes density and fights principle #1.

    // Crease lock: dihedral angle (between face normals) sharper than this → protect.
    let crease_cos = (params.crease_angle_deg.to_radians()).cos().clamp(-1.0, 1.0);
    // Planar mode: only melt edges flatter than this angle (Blender Angle Limit).
    let planar_min_cos = crease_cos;
    let crease_strength = params.crease_strength;
    let boundary_mult = if params.boundary_strength > 1e-6 {
        1.0 + 7.0 * params.boundary_strength
    } else {
        1.0
    };
    let field_boost = 2.0 * params.field_strength; // assist only
    // Flat melt power: planar_bias 1 → ~5, 2.5 → ~9.5 (very aggressive on coplanar).
    let flat_power = (2.0 + 3.0 * params.planar_bias).clamp(2.0, 14.0);
    let max_error = params.max_error;

    let mut positions = mesh.positions;
    let mut indices = mesh.indices;
    let n_vert = positions.len();
    let mut q = vec![Quadric::default(); n_vert];
    // Face normals for dihedral / plane quadrics
    let mut face_n: Vec<Vec3> = Vec::with_capacity(indices.len() / 3);
    for t in indices.chunks_exact(3) {
        let p0 = positions[t[0] as usize];
        let p1 = positions[t[1] as usize];
        let p2 = positions[t[2] as usize];
        let n = (p1 - p0).cross(p2 - p0);
        let nrm = if n.length_squared() > 1e-20 {
            n.normalize()
        } else {
            Vec3::Y
        };
        face_n.push(nrm);
        let pq = Quadric::from_plane(nrm, p0);
        q[t[0] as usize] = q[t[0] as usize].add(pq);
        q[t[1] as usize] = q[t[1] as usize].add(pq);
        q[t[2] as usize] = q[t[2] as usize].add(pq);
    }

    // Decimation Master–style importance: 0 = flat, 1 = high local detail.
    let importance: Vec<f32> = if use_importance {
        build_vertex_importance(n_vert, &indices, &face_n)
    } else {
        Vec::new()
    };

    // Edge → up to 2 face indices (for dihedral crease).
    let mut edge_faces: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (fi, t) in indices.chunks_exact(3).enumerate() {
        for &(a, b) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a < b { (a, b) } else { (b, a) };
            edge_faces.entry(key).or_default().push(fi);
        }
    }

    #[derive(Clone, Copy)]
    struct Cand {
        cost: f32,
        a: u32,
        b: u32,
        gen: u32,
    }
    impl PartialEq for Cand {
        fn eq(&self, o: &Self) -> bool {
            self.cost == o.cost && self.a == o.a && self.b == o.b
        }
    }
    impl Eq for Cand {}
    impl PartialOrd for Cand {
        fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
            Some(self.cmp(o))
        }
    }
    impl Ord for Cand {
        fn cmp(&self, o: &Self) -> Ordering {
            // Min-heap via reverse cost
            o.cost
                .partial_cmp(&self.cost)
                .unwrap_or(Ordering::Equal)
                .then_with(|| self.a.cmp(&o.a))
                .then_with(|| self.b.cmp(&o.b))
        }
    }

    // Per-vertex generation for stale heap entries.
    let mut gen = vec![0u32; n_vert];
    let mut alive = vec![true; n_vert];
    // Union-find style: remap[v] = current live representative
    let mut parent: Vec<u32> = (0..n_vert as u32).collect();
    let find = |parent: &mut [u32], mut x: u32| -> u32 {
        let mut root = x;
        while parent[root as usize] != root {
            root = parent[root as usize];
        }
        while parent[x as usize] != x {
            let p = parent[x as usize];
            parent[x as usize] = root;
            x = p;
        }
        root
    };

    let curv_at = |i: usize| -> f32 {
        curv.and_then(|c| c.get(i).copied()).unwrap_or(0.0)
    };

    let edge_cost = |a: u32,
                     b: u32,
                     positions: &[Vec3],
                     q: &[Quadric],
                     edge_faces: &HashMap<(u32, u32), Vec<usize>>,
                     face_n: &[Vec3]|
     -> (f32, f32, Vec3) {
        // returns (weighted_cost, raw_qem_error, placement)
        let pa = positions[a as usize];
        let pb = positions[b as usize];
        let mid = (pa + pb) * 0.5;
        let len2 = (pa - pb).length_squared().max(1e-20);

        let (raw, p) = if use_qem {
            let qa = q[a as usize].add(q[b as usize]);
            let p_opt = qa.optimal(mid);
            // Clamp placement to segment (stability on creases)
            let t = (p_opt - pa).dot(pb - pa) / len2;
            let p = if (0.0..=1.0).contains(&t) {
                p_opt
            } else {
                mid
            };
            (qa.error(p).max(0.0), p)
        } else {
            // Fast: midpoint only; length is the geometric base.
            (len2, mid)
        };

        // Tiny length term so pure-flat zero-QEM still has a total order (short flats first).
        let base = if use_qem {
            raw + 1e-10 * len2 + 1e-16
        } else {
            len2
        };

        // --- Mesh dihedral flatness (PRIMARY signal for principle #1) ---
        let key = if a < b { (a, b) } else { (b, a) };
        let mut flatness = 0.5f32; // unknown
        let mut curve = 0.5f32;
        let mut cos_n = 0.0f32;
        let mut is_boundary = false;
        if let Some(fs) = edge_faces.get(&key) {
            if fs.len() >= 2 {
                let n0 = face_n[fs[0]];
                let n1 = face_n[fs[1]];
                cos_n = n0.dot(n1).clamp(-1.0, 1.0);
                // flatness 1 = coplanar same-side, 0 = 90°+ fold
                flatness = cos_n.max(0.0);
                curve = ((1.0 - cos_n) * 0.5).clamp(0.0, 1.0);
            } else if fs.len() == 1 {
                is_boundary = true;
                flatness = 0.25;
                curve = 0.5;
            }
        }

        // PRINCIPLE #1: melt flats first → divide by flatness^power
        // flat≈1 → huge denom → tiny cost; flat≈0 → cost stays large.
        let flat_denom = flatness.powf(flat_power) + 1e-5;

        // Protect curvature: quadratic in dihedral deviation
        let mut curve_mul = 1.0 + curve * curve * (10.0 + 50.0 * crease_strength);

        // Hard crease lock (sharp edges stay)
        if !is_boundary && cos_n < crease_cos && crease_strength > 1e-6 {
            let t = ((crease_cos - cos_n) / (1.0 + crease_cos).max(1e-3)).clamp(0.0, 1.0);
            curve_mul *= 1.0 + t * (30.0 + 80.0 * crease_strength);
        }

        // Decimation Master importance map (vertex normal variance).
        if use_importance && !importance.is_empty() {
            let imp = importance
                .get(a as usize)
                .copied()
                .unwrap_or(0.0)
                .max(importance.get(b as usize).copied().unwrap_or(0.0));
            curve_mul *= 1.0 + imp * imp * (14.0 + 45.0 * crease_strength);
        }

        // Optional SDF assist (secondary — mesh dihedral already does the job on DC)
        if field_boost > 1e-6 {
            let hc = curv_at(a as usize).max(curv_at(b as usize));
            if hc > 1e-4 {
                curve_mul *= 1.0 + hc * field_boost;
            }
        }

        // Blender Planar: refuse collapse if edge is not flat enough.
        if planar_only && !is_boundary && cos_n < planar_min_cos {
            return (f32::MAX * 0.25, raw, p);
        }

        let mut cost = base * curve_mul / flat_denom;
        if is_boundary && boundary_mult > 1.0 {
            cost *= boundary_mult;
        }
        // Planar mode: refuse edges sharper than crease angle (only melt flats).
        if planar_only && !is_boundary && cos_n < crease_cos {
            cost = f32::MAX * 0.25;
        }
        // Numerical floor
        if !cost.is_finite() || cost < 0.0 {
            cost = f32::MAX * 0.25;
        }

        (cost, raw, p)
    };

    let mut heap = BinaryHeap::new();
    let push_all = |heap: &mut BinaryHeap<Cand>,
                    indices: &[u32],
                    positions: &[Vec3],
                    q: &[Quadric],
                    parent: &mut [u32],
                    gen: &[u32],
                    alive: &[bool],
                    edge_faces: &HashMap<(u32, u32), Vec<usize>>,
                    face_n: &[Vec3]| {
        heap.clear();
        let mut seen = HashMap::<(u32, u32), ()>::new();
        for t in indices.chunks_exact(3) {
            for &(ra, rb) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                let a = find(parent, ra);
                let b = find(parent, rb);
                if a == b || !alive[a as usize] || !alive[b as usize] {
                    continue;
                }
                let key = if a < b { (a, b) } else { (b, a) };
                if seen.insert(key, ()).is_some() {
                    continue;
                }
                let (cost, _raw, _) = edge_cost(a, b, positions, q, edge_faces, face_n);
                heap.push(Cand {
                    cost,
                    a: key.0,
                    b: key.1,
                    gen: gen[key.0 as usize].wrapping_add(gen[key.1 as usize]),
                });
            }
        }
    };

    push_all(
        &mut heap,
        &indices,
        &positions,
        &q,
        &mut parent,
        &gen,
        &alive,
        &edge_faces,
        &face_n,
    );

    // Safety: never O(n^2) per collapse. Cap work so UI add-Decimate cannot freeze/crash.
    let need = n_tri.saturating_sub(target_tri);
    let max_collapses = need.min(n_tri).min(200_000);
    let mut collapses = 0usize;
    let mut stale = 0usize;
    let mut live_tris = n_tri;
    let mut no_progress_rebuilds = 0u32;

    while collapses < max_collapses {
        if live_tris <= target_tri {
            break;
        }
        if heap.is_empty() || stale > 128 {
            // Compact indices via union-find; rebuild face topology.
            let mut clean = Vec::with_capacity(indices.len());
            for t in indices.chunks_exact(3) {
                let ta = find(&mut parent, t[0]);
                let tb = find(&mut parent, t[1]);
                let tc = find(&mut parent, t[2]);
                if ta != tb && tb != tc && ta != tc
                    && (ta as usize) < n_vert
                    && (tb as usize) < n_vert
                    && (tc as usize) < n_vert
                {
                    clean.extend_from_slice(&[ta, tb, tc]);
                }
            }
            let prev_len = indices.len();
            indices = clean;
            live_tris = indices.len() / 3;
            if live_tris <= target_tri {
                break;
            }
            if indices.len() >= prev_len {
                no_progress_rebuilds += 1;
                if no_progress_rebuilds >= 3 {
                    break; // cannot reduce further
                }
            } else {
                no_progress_rebuilds = 0;
            }
            face_n.clear();
            edge_faces.clear();
            face_n.reserve(live_tris);
            for (fi, t) in indices.chunks_exact(3).enumerate() {
                let p0 = positions[t[0] as usize];
                let p1 = positions[t[1] as usize];
                let p2 = positions[t[2] as usize];
                let n = (p1 - p0).cross(p2 - p0);
                let nrm = if n.length_squared() > 1e-20 {
                    n.normalize()
                } else {
                    Vec3::Y
                };
                face_n.push(nrm);
                for &(ea, eb) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                    let key = if ea < eb { (ea, eb) } else { (eb, ea) };
                    edge_faces.entry(key).or_default().push(fi);
                }
            }
            push_all(
                &mut heap,
                &indices,
                &positions,
                &q,
                &mut parent,
                &gen,
                &alive,
                &edge_faces,
                &face_n,
            );
            stale = 0;
            if heap.is_empty() {
                break;
            }
            continue;
        }

        let Some(cand) = heap.pop() else { break };
        // Refuse impossible planar costs immediately (stop thrashing).
        if planar_only && cand.cost >= f32::MAX * 0.2 {
            no_progress_rebuilds += 1;
            if no_progress_rebuilds >= 2 {
                break;
            }
            stale = 999; // force rebuild then exit
            continue;
        }

        let a = find(&mut parent, cand.a);
        let b = find(&mut parent, cand.b);
        if a == b
            || (a as usize) >= n_vert
            || (b as usize) >= n_vert
            || !alive[a as usize]
            || !alive[b as usize]
        {
            stale += 1;
            continue;
        }
        let gsum = gen[a as usize].wrapping_add(gen[b as usize]);
        if gsum != cand.gen {
            stale += 1;
            continue;
        }

        let (cost, raw_err, p_new) = edge_cost(a, b, &positions, &q, &edge_faces, &face_n);
        if !cost.is_finite() || cost >= f32::MAX * 0.2 {
            stale += 1;
            if planar_only {
                break;
            }
            continue;
        }
        if cost > cand.cost * 8.0 + 1e-6 && cand.cost > 1e-12 {
            stale += 1;
            continue;
        }
        if max_error > 0.0 && raw_err > max_error {
            stale += 1;
            if stale > 64 {
                break;
            }
            continue;
        }

        // Cheap local flip guard: only the (up to 2) faces of this edge — NOT whole mesh.
        let key = if a < b { (a, b) } else { (b, a) };
        let mut flip = false;
        if let Some(fs) = edge_faces.get(&key) {
            for &fi in fs.iter().take(2) {
                if fi >= face_n.len() {
                    continue;
                }
                let base = fi * 3;
                if base + 2 >= indices.len() {
                    continue;
                }
                let t = &indices[base..base + 3];
                let ia = find(&mut parent, t[0]);
                let ib = find(&mut parent, t[1]);
                let ic = find(&mut parent, t[2]);
                let mut v = [ia, ib, ic];
                for x in &mut v {
                    if *x == b {
                        *x = a;
                    }
                }
                if v[0] == v[1] || v[1] == v[2] || v[0] == v[2] {
                    continue;
                }
                if (v[0] as usize) >= n_vert
                    || (v[1] as usize) >= n_vert
                    || (v[2] as usize) >= n_vert
                {
                    continue;
                }
                let p0 = if v[0] == a { p_new } else { positions[v[0] as usize] };
                let p1 = if v[1] == a { p_new } else { positions[v[1] as usize] };
                let p2 = if v[2] == a { p_new } else { positions[v[2] as usize] };
                let n_old = (positions[ib as usize] - positions[ia as usize])
                    .cross(positions[ic as usize] - positions[ia as usize]);
                let n_new = (p1 - p0).cross(p2 - p0);
                if n_old.length_squared() > 1e-16
                    && n_new.length_squared() > 1e-16
                    && n_old.dot(n_new) < 0.0
                {
                    flip = true;
                    break;
                }
            }
        }
        if flip {
            stale += 1;
            continue;
        }

        // Commit: union-find only (NO full index rewrite — was O(n) per collapse → freeze/crash).
        if !p_new.is_finite() {
            stale += 1;
            continue;
        }
        positions[a as usize] = p_new;
        q[a as usize] = q[a as usize].add(q[b as usize]);
        parent[b as usize] = a;
        alive[b as usize] = false;
        gen[a as usize] = gen[a as usize].wrapping_add(1);
        gen[b as usize] = gen[b as usize].wrapping_add(1);
        collapses += 1;
        stale = 0;
        // Two faces typically removed per manifold edge collapse.
        live_tris = live_tris.saturating_sub(2);

        // Periodic topology rebuild; local edge inject is skipped (stale edge_faces).
        if collapses % 48 == 0 {
            heap.clear();
            stale = 999; // trigger rebuild at top
        }
    }

    // Final compact
    let mut clean = Vec::with_capacity(indices.len());
    for t in indices.chunks_exact(3) {
        let a = find(&mut parent, t[0]);
        let b = find(&mut parent, t[1]);
        let c = find(&mut parent, t[2]);
        if a != b && b != c && a != c {
            clean.extend_from_slice(&[a, b, c]);
        }
    }
    indices = clean;

    let n = positions.len();
    let mut used = vec![false; n];
    for &i in &indices {
        if (i as usize) < n {
            used[i as usize] = true;
        }
    }
    let mut remap = vec![u32::MAX; n];
    let mut new_pos = Vec::new();
    for i in 0..n {
        if used[i] {
            remap[i] = new_pos.len() as u32;
            new_pos.push(positions[i]);
        }
    }
    let mut final_idx = Vec::with_capacity(indices.len());
    for t in indices.chunks_exact(3) {
        let a = remap[t[0] as usize];
        let b = remap[t[1] as usize];
        let c = remap[t[2] as usize];
        if a != u32::MAX && b != u32::MAX && c != u32::MAX && a != b && b != c && a != c {
            final_idx.extend_from_slice(&[a, b, c]);
        }
    }
    let mut out = DisplayMesh {
        positions: new_pos,
        normals: vec![Vec3::Y; 0],
        indices: final_idx,
    };
    out.normals = vec![Vec3::Y; out.positions.len()];
    mesh_recompute_normals(out)
}

/// O(n) cluster decimate (legacy / emergency). Uniform cells — **not** flat-first.
/// Kept for experiments; production modes no longer call this.
#[allow(dead_code)]
fn mesh_decimate_cluster(mesh: DisplayMesh, target_tri: usize) -> DisplayMesh {
    use std::collections::HashMap;

    let n_tri = mesh.triangle_count().max(1);
    let ratio = (target_tri as f32 / n_tri as f32).clamp(0.05, 1.0);
    // Estimate average edge length from a sample of triangles.
    let sample = n_tri.min(2_000);
    let mut edge_sum = 0.0f32;
    let mut edge_n = 0u32;
    for t in mesh.indices.chunks_exact(3).take(sample) {
        let p0 = mesh.positions[t[0] as usize];
        let p1 = mesh.positions[t[1] as usize];
        let p2 = mesh.positions[t[2] as usize];
        edge_sum += (p0 - p1).length() + (p1 - p2).length() + (p2 - p0).length();
        edge_n += 3;
    }
    let avg_edge = if edge_n > 0 {
        (edge_sum / edge_n as f32).max(1e-8)
    } else {
        1e-3
    };
    // Linear scale on surface ∝ sqrt(area ratio) ≈ sqrt(tri ratio).
    let cell = (avg_edge / ratio.sqrt().max(0.05)).max(avg_edge * 1.01);
    let inv = 1.0 / cell;

    let mut cell_to_out: HashMap<(i32, i32, i32), u32> = HashMap::with_capacity(mesh.positions.len() / 2);
    let mut new_pos: Vec<Vec3> = Vec::new();
    let mut new_nrm: Vec<Vec3> = Vec::new();
    let mut counts: Vec<u32> = Vec::new();
    let mut remap = vec![0u32; mesh.positions.len()];

    for (i, &p) in mesh.positions.iter().enumerate() {
        let key = (
            (p.x * inv).floor() as i32,
            (p.y * inv).floor() as i32,
            (p.z * inv).floor() as i32,
        );
        let nrm = mesh.normals.get(i).copied().unwrap_or(Vec3::Y);
        if let Some(&j) = cell_to_out.get(&key) {
            remap[i] = j;
            new_pos[j as usize] += p;
            new_nrm[j as usize] += nrm;
            counts[j as usize] += 1;
        } else {
            let j = new_pos.len() as u32;
            cell_to_out.insert(key, j);
            remap[i] = j;
            new_pos.push(p);
            new_nrm.push(nrm);
            counts.push(1);
        }
    }
    for i in 0..new_pos.len() {
        let c = counts[i].max(1) as f32;
        new_pos[i] /= c;
        new_nrm[i] = (new_nrm[i] / c).normalize_or_zero();
        if new_nrm[i].length_squared() < 1e-12 {
            new_nrm[i] = Vec3::Y;
        }
    }
    let mut clean = Vec::with_capacity(mesh.indices.len());
    for t in mesh.indices.chunks_exact(3) {
        let a = remap[t[0] as usize];
        let b = remap[t[1] as usize];
        let c = remap[t[2] as usize];
        if a != b && b != c && a != c {
            clean.extend_from_slice(&[a, b, c]);
        }
    }
    // If still above target (clustering too fine), one more coarser pass.
    let mut out = DisplayMesh {
        positions: new_pos,
        normals: new_nrm,
        indices: clean,
    };
    if out.triangle_count() > target_tri.saturating_mul(2).max(target_tri + 100) {
        // Coarsen cell ×2 via recursive call with adjusted target (use edge heap only if small enough)
        if out.triangle_count() <= 25_000 {
            out = mesh_decimate_edge_heap(out, target_tri);
        } else {
            // Second cluster pass at 2× cell effectively via ratio re-estimate
            let r2 = (target_tri as f32 / out.triangle_count() as f32).clamp(0.05, 1.0);
            out = mesh_decimate_cluster_once(out, r2);
        }
    }
    if out.normals.len() != out.positions.len() {
        out.normals = vec![Vec3::Y; out.positions.len()];
    }
    mesh_recompute_normals(out)
}

/// Single spatial cluster pass with ratio-based cell size (no recursion).
#[allow(dead_code)]
fn mesh_decimate_cluster_once(mesh: DisplayMesh, ratio: f32) -> DisplayMesh {
    use std::collections::HashMap;
    let ratio = ratio.clamp(0.05, 1.0);
    let sample = mesh.triangle_count().min(2_000).max(1);
    let mut edge_sum = 0.0f32;
    let mut edge_n = 0u32;
    for t in mesh.indices.chunks_exact(3).take(sample) {
        let p0 = mesh.positions[t[0] as usize];
        let p1 = mesh.positions[t[1] as usize];
        let p2 = mesh.positions[t[2] as usize];
        edge_sum += (p0 - p1).length() + (p1 - p2).length() + (p2 - p0).length();
        edge_n += 3;
    }
    let avg_edge = (edge_sum / edge_n.max(1) as f32).max(1e-8);
    let cell = (avg_edge / ratio.sqrt().max(0.05)).max(avg_edge * 1.01);
    let inv = 1.0 / cell;
    let mut cell_to_out: HashMap<(i32, i32, i32), u32> = HashMap::new();
    let mut new_pos = Vec::new();
    let mut counts = Vec::new();
    let mut remap = vec![0u32; mesh.positions.len()];
    for (i, &p) in mesh.positions.iter().enumerate() {
        let key = (
            (p.x * inv).floor() as i32,
            (p.y * inv).floor() as i32,
            (p.z * inv).floor() as i32,
        );
        if let Some(&j) = cell_to_out.get(&key) {
            remap[i] = j;
            new_pos[j as usize] += p;
            counts[j as usize] += 1;
        } else {
            let j = new_pos.len() as u32;
            cell_to_out.insert(key, j);
            remap[i] = j;
            new_pos.push(p);
            counts.push(1u32);
        }
    }
    for i in 0..new_pos.len() {
        new_pos[i] /= counts[i].max(1) as f32;
    }
    let mut clean = Vec::new();
    for t in mesh.indices.chunks_exact(3) {
        let a = remap[t[0] as usize];
        let b = remap[t[1] as usize];
        let c = remap[t[2] as usize];
        if a != b && b != c && a != c {
            clean.extend_from_slice(&[a, b, c]);
        }
    }
    let mut out = DisplayMesh {
        positions: new_pos,
        normals: vec![Vec3::Y; 0],
        indices: clean,
    };
    out.normals = vec![Vec3::Y; out.positions.len()];
    mesh_recompute_normals(out)
}

/// Binary-heap short-edge collapse until `target_tri` (legacy, not flat-first).
#[allow(dead_code)]
fn mesh_decimate_edge_heap(mesh: DisplayMesh, target_tri: usize) -> DisplayMesh {
    use std::cmp::Ordering;
    use std::collections::BinaryHeap;

    #[derive(Clone, Copy)]
    struct EdgeCand {
        len2: f32,
        a: u32,
        b: u32,
    }
    impl PartialEq for EdgeCand {
        fn eq(&self, o: &Self) -> bool {
            self.len2 == o.len2 && self.a == o.a && self.b == o.b
        }
    }
    impl Eq for EdgeCand {}
    impl PartialOrd for EdgeCand {
        fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
            Some(self.cmp(o))
        }
    }
    impl Ord for EdgeCand {
        fn cmp(&self, o: &Self) -> Ordering {
            // Min-heap via reverse: shorter edges first
            o.len2.partial_cmp(&self.len2).unwrap_or(Ordering::Equal)
                .then_with(|| self.a.cmp(&o.a))
                .then_with(|| self.b.cmp(&o.b))
        }
    }

    let mut positions = mesh.positions;
    let mut indices = mesh.indices;
    let mut heap = BinaryHeap::new();

    let push_edges = |indices: &[u32], positions: &[Vec3], heap: &mut BinaryHeap<EdgeCand>| {
        heap.clear();
        for t in indices.chunks_exact(3) {
            for &(a, b) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                if a == b {
                    continue;
                }
                let (lo, hi) = if a < b { (a, b) } else { (b, a) };
                let l2 = (positions[lo as usize] - positions[hi as usize]).length_squared();
                heap.push(EdgeCand {
                    len2: l2,
                    a: lo,
                    b: hi,
                });
            }
        }
    };
    // Safety: absolute max collapses = current tri count (honest bound, not silent under-cap).
    let max_collapses = indices.len() / 3;
    let mut collapses = 0usize;
    let mut stale_skips = 0usize;

    while collapses < max_collapses {
        let tri_now = indices.len() / 3;
        if tri_now <= target_tri {
            break;
        }
        // Rebuild heap when empty or too many stale pops (keeps progress toward target).
        if heap.is_empty() || stale_skips > 128 {
            push_edges(&indices, &positions, &mut heap);
            stale_skips = 0;
            if heap.is_empty() {
                break;
            }
        }
        let Some(cand) = heap.pop() else {
            break;
        };
        let (keep, drop) = (cand.a, cand.b);
        if keep == drop
            || keep as usize >= positions.len()
            || drop as usize >= positions.len()
        {
            stale_skips += 1;
            continue;
        }
        // Stale check: edge no longer present at that length (vert already merged away).
        let live = (positions[keep as usize] - positions[drop as usize]).length_squared();
        if live > cand.len2 * 4.0 + 1e-8 {
            stale_skips += 1;
            continue;
        }
        // Also require both verts still used in indices
        let mut used_keep = false;
        let mut used_drop = false;
        for &idx in &indices {
            if idx == keep {
                used_keep = true;
            }
            if idx == drop {
                used_drop = true;
            }
        }
        if !used_keep || !used_drop {
            stale_skips += 1;
            continue;
        }
        let mid = (positions[keep as usize] + positions[drop as usize]) * 0.5;
        positions[keep as usize] = mid;
        for idx in indices.iter_mut() {
            if *idx == drop {
                *idx = keep;
            }
        }
        let mut clean = Vec::with_capacity(indices.len());
        for t in indices.chunks_exact(3) {
            if t[0] != t[1] && t[1] != t[2] && t[0] != t[2] {
                clean.extend_from_slice(t);
            }
        }
        if clean.len() >= indices.len() {
            break;
        }
        indices = clean;
        collapses += 1;
        stale_skips = 0;
        // Push new edges incident to keep (approx) — full rebuild every 32 collapses is simpler/correct
        if collapses % 32 == 0 {
            push_edges(&indices, &positions, &mut heap);
        }
    }

    // Compact verts
    let n = positions.len();
    let mut used = vec![false; n];
    for &i in &indices {
        if (i as usize) < n {
            used[i as usize] = true;
        }
    }
    let mut remap = vec![u32::MAX; n];
    let mut new_pos = Vec::new();
    for i in 0..n {
        if used[i] {
            remap[i] = new_pos.len() as u32;
            new_pos.push(positions[i]);
        }
    }
    let mut final_idx = Vec::with_capacity(indices.len());
    for t in indices.chunks_exact(3) {
        let a = remap[t[0] as usize];
        let b = remap[t[1] as usize];
        let c = remap[t[2] as usize];
        if a != u32::MAX && b != u32::MAX && c != u32::MAX && a != b && b != c && a != c {
            final_idx.extend_from_slice(&[a, b, c]);
        }
    }
    let mut out = DisplayMesh {
        positions: new_pos,
        normals: vec![Vec3::Y; 0],
        indices: final_idx,
    };
    out.normals = vec![Vec3::Y; out.positions.len()];
    mesh_recompute_normals(out)
}
