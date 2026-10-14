// chunked_dc/stitch.rs -- merge per-chunk meshes into a unified mesh.
// GOD_FILES_SPLIT_PLAN: chunked_dc/{mod,grid,extract,stitch,tests}.rs
// Pure relocation of ChunkedMesh::merge_to_mesh / to_mesh (determinism sort travels).

use glam::Vec3;

use super::extract::ChunkedMesh;

impl ChunkedMesh {
    /// Merge all chunks into a single unified mesh.
    /// Concatenates vertices/normals/quads, adjusting quad indices for each chunk's offset.
    pub fn merge_to_mesh(&self) -> (Vec<Vec3>, Vec<Vec3>, Vec<[u32; 4]>) {
        // Pre-count for capacity
        let total_verts: usize = self.chunks.values().map(|c| c.vertices.len()).sum();
        let total_quads: usize = self.chunks.values().map(|c| c.quads.len()).sum();

        let mut vertices = Vec::with_capacity(total_verts);
        let mut normals = Vec::with_capacity(total_verts);
        let mut quads = Vec::with_capacity(total_quads);

        // DETERMINISM (Gate 0 / DC-consolidation): `self.chunks` is a `HashMap`, whose value-iteration
        // order is randomized per instance (`RandomState`). Concatenating chunks in that order made the
        // merged vert/idx byte stream NON-DETERMINISTIC run-to-run — same vertex/quad SET, shuffled
        // order + shifted `base` offsets. That defeats a golden mesh hash (the DC-consolidation safety
        // brick) and makes the CPU-fallback DC (`app_poll_gpu.rs`) and the SDF→mesh export/convert
        // (`convert.rs`) non-reproducible (the same scene exports different vertex order each run).
        // Emit chunks in sorted `(cx,cy,cz)` order: byte-stable output, IDENTICAL geometry (vertex
        // order is immaterial to the drawn/exported surface — same triangles, same positions). This is
        // the minimal geometry-preserving sort; it changes ordering only. See the CLAUDE.md rule
        // "HashMap iteration = random order — sort when order matters".
        let mut coords: Vec<(u32, u32, u32)> = self.chunks.keys().copied().collect();
        coords.sort_unstable();
        for coord in coords {
            let chunk = &self.chunks[&coord];
            let base = vertices.len() as u32;
            vertices.extend_from_slice(&chunk.vertices);
            normals.extend_from_slice(&chunk.normals);
            for q in &chunk.quads {
                quads.push([q[0] + base, q[1] + base, q[2] + base, q[3] + base]);
            }
        }

        (vertices, normals, quads)
    }

    /// Convert merged output into a `Mesh` struct.
    pub fn to_mesh(&self) -> crate::mesh::Mesh {
        let (vertices, normals, quads) = self.merge_to_mesh();
        crate::mesh::Mesh { vertices, normals, quads }
    }
}
