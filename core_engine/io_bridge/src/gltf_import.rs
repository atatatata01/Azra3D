// ============================================================
// glTF 2.0 Import
// ============================================================
//
// Imports .gltf / .glb files and extracts meshes + PBR materials.
// Returns ImportedMeshData structs ready for direct insertion
// into the mesh cache (no Dual Contouring needed).

use glam::Vec3;

// ============================================================
// Result types
// ============================================================

/// A single imported mesh primitive with its vertex data.
pub struct ImportedMeshData {
    pub name: String,
    /// World-space positions.
    pub positions: Vec<Vec3>,
    /// Per-vertex normals (generated if not in file).
    pub normals: Vec<Vec3>,
    /// Per-vertex UVs (zeros if not in file).
    pub uvs: Vec<[f32; 2]>,
    /// Triangle indices.
    pub indices: Vec<u32>,
    /// Index into `GltfImportResult.materials` (None = default).
    pub material_idx: Option<usize>,
}

/// PBR material extracted from glTF.
pub struct GltfPbrMaterial {
    pub name: String,
    pub albedo: Vec3,
    pub opacity: f32,
    pub roughness: f32,
    pub metallic: f32,
    pub emission_color: Vec3,
    pub emission_intensity: f32,
}

impl Default for GltfPbrMaterial {
    fn default() -> Self {
        Self {
            name: String::new(),
            albedo: Vec3::new(0.8, 0.8, 0.8),
            opacity: 1.0,
            roughness: 0.5,
            metallic: 0.0,
            emission_color: Vec3::ZERO,
            emission_intensity: 0.0,
        }
    }
}

/// Complete result of a glTF import.
pub struct GltfImportResult {
    pub meshes: Vec<ImportedMeshData>,
    pub materials: Vec<GltfPbrMaterial>,
    pub root_name: String,
}

// ============================================================
// Import function
// ============================================================

/// Import a glTF 2.0 file (.gltf or .glb).
///
/// Returns mesh primitives and PBR materials extracted from the scene.
/// Meshes are returned in world space (node transforms applied).
pub fn import_gltf(path: &str) -> Result<GltfImportResult, String> {
    let (document, buffers, _images) =
        gltf::import(path).map_err(|e| format!("glTF load failed: {e}"))?;

    let root_name = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("imported")
        .to_string();

    // --- Extract materials ---
    let materials: Vec<GltfPbrMaterial> = document
        .materials()
        .map(|mat| {
            let pbr = mat.pbr_metallic_roughness();
            let base = pbr.base_color_factor();
            let em = mat.emissive_factor();
            let em_strength = 1.0f32; // KHR_materials_emissive_strength not exposed in gltf 1.x
            let em_lum = em[0] * 0.2126 + em[1] * 0.7152 + em[2] * 0.0722;
            GltfPbrMaterial {
                name: mat.name().unwrap_or("").to_string(),
                albedo: Vec3::new(base[0], base[1], base[2]),
                opacity: base[3],
                roughness: pbr.roughness_factor(),
                metallic: pbr.metallic_factor(),
                emission_color: if em_lum > 0.001 {
                    // Normalize emission color, intensity carries the magnitude
                    let max_c = em[0].max(em[1]).max(em[2]).max(0.001);
                    Vec3::new(em[0] / max_c, em[1] / max_c, em[2] / max_c)
                } else {
                    Vec3::ZERO
                },
                emission_intensity: em_lum * em_strength,
            }
        })
        .collect();

    // --- Walk scene nodes and extract mesh primitives ---
    let mut meshes = Vec::new();

    for scene in document.scenes() {
        for node in scene.nodes() {
            collect_node_meshes(
                &node,
                glam::Mat4::IDENTITY,
                &buffers,
                &mut meshes,
            );
        }
    }

    Ok(GltfImportResult {
        meshes,
        materials,
        root_name,
    })
}

/// Recursively walk a glTF node tree, accumulating transforms.
fn collect_node_meshes(
    node: &gltf::Node,
    parent_transform: glam::Mat4,
    buffers: &[gltf::buffer::Data],
    out: &mut Vec<ImportedMeshData>,
) {
    // Compute this node's world transform
    let local = glam::Mat4::from_cols_array_2d(&node.transform().matrix());
    let world = parent_transform * local;

    // Extract meshes from this node
    if let Some(mesh) = node.mesh() {
        let mesh_name = mesh.name().unwrap_or("mesh").to_string();

        for (prim_idx, primitive) in mesh.primitives().enumerate() {
            let name = if mesh.primitives().count() > 1 {
                format!("{}.{}", mesh_name, prim_idx)
            } else {
                mesh_name.clone()
            };

            if let Some(data) = extract_primitive(&primitive, buffers, &world) {
                let mut imported = data;
                imported.name = name;
                imported.material_idx = primitive.material().index();
                out.push(imported);
            }
        }
    }

    // Recurse into children
    for child in node.children() {
        collect_node_meshes(&child, world, buffers, out);
    }
}

/// Extract vertex data from a single glTF mesh primitive.
fn extract_primitive(
    primitive: &gltf::Primitive,
    buffers: &[gltf::buffer::Data],
    world_transform: &glam::Mat4,
) -> Option<ImportedMeshData> {
    // Only handle triangle primitives
    if primitive.mode() != gltf::mesh::Mode::Triangles {
        return None;
    }

    let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(|v| &**v));

    // Positions (required)
    let positions: Vec<Vec3> = reader
        .read_positions()?
        .map(|p| {
            let wp = *world_transform * glam::Vec4::new(p[0], p[1], p[2], 1.0);
            Vec3::new(wp.x, wp.y, wp.z)
        })
        .collect();

    if positions.is_empty() {
        return None;
    }

    // Indices (required for indexed geometry, flatten if non-indexed)
    let indices: Vec<u32> = if let Some(idx) = reader.read_indices() {
        idx.into_u32().collect()
    } else {
        // Non-indexed: sequential 0..n
        (0..positions.len() as u32).collect()
    };

    // Normal matrix for transforming normals
    let normal_mat = glam::Mat3::from_mat4(*world_transform)
        .inverse()
        .transpose();

    // Normals (optional — generate flat normals if missing)
    let normals: Vec<Vec3> = if let Some(norms) = reader.read_normals() {
        norms
            .map(|n| (normal_mat * Vec3::new(n[0], n[1], n[2])).normalize_or_zero())
            .collect()
    } else {
        // Compute flat normals from triangle faces
        let mut flat_normals = vec![Vec3::Y; positions.len()];
        for tri in indices.chunks_exact(3) {
            let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            if i0 < positions.len() && i1 < positions.len() && i2 < positions.len() {
                let e1 = positions[i1] - positions[i0];
                let e2 = positions[i2] - positions[i0];
                let n = e1.cross(e2).normalize_or_zero();
                flat_normals[i0] = n;
                flat_normals[i1] = n;
                flat_normals[i2] = n;
            }
        }
        flat_normals
    };

    // UVs (optional)
    let uvs: Vec<[f32; 2]> = if let Some(tex) = reader.read_tex_coords(0) {
        tex.into_f32().collect()
    } else {
        vec![[0.0, 0.0]; positions.len()]
    };

    Some(ImportedMeshData {
        name: String::new(), // filled by caller
        positions,
        normals,
        uvs,
        indices,
        material_idx: None, // filled by caller
    })
}

// ============================================================
// Conversion to engine Mesh format
// ============================================================

impl ImportedMeshData {
    /// Convert to GPU-ready raster data: `[px, py, pz, nx, ny, nz, u, v]` + triangle indices.
    ///
    /// Same format as `Mesh::to_raster_data()`.
    pub fn to_raster_data(&self) -> (Vec<[f32; 8]>, Vec<u32>) {
        let verts: Vec<[f32; 8]> = (0..self.positions.len())
            .map(|i| {
                let p = self.positions[i];
                let n = if i < self.normals.len() {
                    self.normals[i]
                } else {
                    Vec3::Y
                };
                let uv = if i < self.uvs.len() {
                    self.uvs[i]
                } else {
                    [0.0, 0.0]
                };
                [p.x, p.y, p.z, n.x, n.y, n.z, uv[0], uv[1]]
            })
            .collect();
        (verts, self.indices.clone())
    }

    /// Triangle count.
    pub fn triangle_count(&self) -> u32 {
        self.indices.len() as u32 / 3
    }
}
