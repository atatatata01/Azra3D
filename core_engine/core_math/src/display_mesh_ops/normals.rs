use glam::Vec3;

use super::DisplayMesh;

pub(super) fn mesh_recompute_normals(mut mesh: DisplayMesh) -> DisplayMesh {
    let n = mesh.positions.len();
    if n == 0 {
        return mesh;
    }
    let mut acc = vec![Vec3::ZERO; n];
    for t in mesh.indices.chunks_exact(3) {
        let (i0, i1, i2) = (t[0] as usize, t[1] as usize, t[2] as usize);
        if i0 >= n || i1 >= n || i2 >= n {
            continue;
        }
        let p0 = mesh.positions[i0];
        let p1 = mesh.positions[i1];
        let p2 = mesh.positions[i2];
        let fnm = (p1 - p0).cross(p2 - p0);
        // area-weighted (cross length ∝ area)
        acc[i0] += fnm;
        acc[i1] += fnm;
        acc[i2] += fnm;
    }
    mesh.normals = acc
        .into_iter()
        .map(|v| {
            let n = v.normalize_or_zero();
            if n.length_squared() < 1e-12 {
                Vec3::Y
            } else {
                n
            }
        })
        .collect();
    mesh
}
