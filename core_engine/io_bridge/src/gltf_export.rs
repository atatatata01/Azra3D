// ============================================================
// glTF 2.0 Binary Export (.glb)
// ============================================================
//
// Writes meshes as a single .glb file (binary glTF 2.0).
// Supports multiple meshes with positions, normals, UVs, and PBR materials.

use glam::Vec3;
use std::io::Write;

/// A mesh to be exported.
pub struct ExportMesh {
    pub name: String,
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub material: ExportMaterial,
}

/// PBR material for export.
pub struct ExportMaterial {
    pub name: String,
    pub albedo: [f32; 4],    // RGBA
    pub roughness: f32,
    pub metallic: f32,
    pub emission: [f32; 3],
}

impl Default for ExportMaterial {
    fn default() -> Self {
        Self {
            name: "default".into(),
            albedo: [0.8, 0.8, 0.8, 1.0],
            roughness: 0.5,
            metallic: 0.0,
            emission: [0.0; 3],
        }
    }
}

/// Export one or more meshes as a .glb file.
pub fn export_glb(path: &str, meshes: &[ExportMesh]) -> Result<(usize, usize), String> {
    if meshes.is_empty() {
        return Err("No meshes to export".into());
    }

    let mut total_verts = 0usize;
    let mut total_faces = 0usize;

    // Build binary buffer: interleaved [positions | normals | uvs | indices] per mesh
    let mut bin = Vec::<u8>::new();
    let mut buffer_views = Vec::new();
    let mut accessors = Vec::new();
    let mut mesh_json_parts = Vec::new();
    let mut material_json_parts = Vec::new();
    let mut exported_names = Vec::new();

    let mut exported_mesh_count = 0usize;
    for mesh in meshes.iter() {
        let vert_count = mesh.positions.len();
        if vert_count == 0 {
            eprintln!("[gltf_export] WARNING: skipping mesh '{}' — 0 vertices", mesh.name);
            continue;
        }
        let mi = exported_mesh_count;
        let idx_count = mesh.indices.len();
        total_verts += vert_count;
        total_faces += idx_count / 3;

        // Compute AABB for positions
        let (mut min_p, mut max_p) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for &p in &mesh.positions {
            min_p = min_p.min(p);
            max_p = max_p.max(p);
        }
        // Bug 1 fix: NaN/Inf positions produce invalid glTF JSON — clamp to zero
        if !min_p.is_finite() || !max_p.is_finite() {
            eprintln!("[gltf_export] WARNING: mesh '{}' has NaN/Inf positions — clamping AABB to zero", mesh.name);
            min_p = Vec3::ZERO;
            max_p = Vec3::ZERO;
        }

        let bv_base = buffer_views.len();
        let acc_base = accessors.len();

        // --- Positions ---
        let pos_offset = bin.len();
        for &p in &mesh.positions {
            bin.extend_from_slice(&p.x.to_le_bytes());
            bin.extend_from_slice(&p.y.to_le_bytes());
            bin.extend_from_slice(&p.z.to_le_bytes());
        }
        let pos_len = bin.len() - pos_offset;
        buffer_views.push(format!(
            r#"{{"buffer":0,"byteOffset":{},"byteLength":{},"target":34962}}"#,
            pos_offset, pos_len
        ));
        accessors.push(format!(
            r#"{{"bufferView":{},"componentType":5126,"count":{},"type":"VEC3","min":[{},{},{}],"max":[{},{},{}]}}"#,
            bv_base, vert_count,
            json_f32(min_p.x), json_f32(min_p.y), json_f32(min_p.z),
            json_f32(max_p.x), json_f32(max_p.y), json_f32(max_p.z)
        ));

        // --- Normals ---
        let norm_offset = bin.len();
        for i in 0..vert_count {
            let n = if i < mesh.normals.len() { mesh.normals[i] } else { Vec3::Y };
            bin.extend_from_slice(&n.x.to_le_bytes());
            bin.extend_from_slice(&n.y.to_le_bytes());
            bin.extend_from_slice(&n.z.to_le_bytes());
        }
        let norm_len = bin.len() - norm_offset;
        buffer_views.push(format!(
            r#"{{"buffer":0,"byteOffset":{},"byteLength":{},"target":34962}}"#,
            norm_offset, norm_len
        ));
        accessors.push(format!(
            r#"{{"bufferView":{},"componentType":5126,"count":{},"type":"VEC3"}}"#,
            bv_base + 1, vert_count
        ));

        // --- UVs ---
        let uv_offset = bin.len();
        for i in 0..vert_count {
            let uv = if i < mesh.uvs.len() { mesh.uvs[i] } else { [0.0, 0.0] };
            bin.extend_from_slice(&uv[0].to_le_bytes());
            bin.extend_from_slice(&uv[1].to_le_bytes());
        }
        let uv_len = bin.len() - uv_offset;
        buffer_views.push(format!(
            r#"{{"buffer":0,"byteOffset":{},"byteLength":{},"target":34962}}"#,
            uv_offset, uv_len
        ));
        accessors.push(format!(
            r#"{{"bufferView":{},"componentType":5126,"count":{},"type":"VEC2"}}"#,
            bv_base + 2, vert_count
        ));

        // --- Indices (u32) ---
        // Align to 4 bytes (u32 already aligned since each prior section is float-aligned)
        let idx_offset = bin.len();
        for &idx in &mesh.indices {
            bin.extend_from_slice(&idx.to_le_bytes());
        }
        let idx_len = bin.len() - idx_offset;
        buffer_views.push(format!(
            r#"{{"buffer":0,"byteOffset":{},"byteLength":{},"target":34963}}"#,
            idx_offset, idx_len
        ));
        accessors.push(format!(
            r#"{{"bufferView":{},"componentType":5125,"count":{},"type":"SCALAR"}}"#,
            bv_base + 3, idx_count
        ));

        // Mesh JSON
        let mesh_name = json_escape(&mesh.name);
        mesh_json_parts.push(format!(
            r#"{{"name":"{}","primitives":[{{"attributes":{{"POSITION":{},"NORMAL":{},"TEXCOORD_0":{}}},"indices":{},"material":{}}}]}}"#,
            mesh_name,
            acc_base,      // POSITION accessor
            acc_base + 1,  // NORMAL accessor
            acc_base + 2,  // TEXCOORD_0 accessor
            acc_base + 3,  // indices accessor
            mi             // material index
        ));

        // Material JSON
        let mat = &mesh.material;
        let mat_name = json_escape(&mat.name);
        let has_emission = mat.emission[0] > 0.001 || mat.emission[1] > 0.001 || mat.emission[2] > 0.001;
        let emission_part = if has_emission {
            format!(r#","emissiveFactor":[{},{},{}]"#, json_f32(mat.emission[0]), json_f32(mat.emission[1]), json_f32(mat.emission[2]))
        } else {
            String::new()
        };
        material_json_parts.push(format!(
            r#"{{"name":"{}","pbrMetallicRoughness":{{"baseColorFactor":[{},{},{},{}],"roughnessFactor":{},"metallicFactor":{}}}{}}}"#,
            mat_name,
            json_f32(mat.albedo[0]), json_f32(mat.albedo[1]), json_f32(mat.albedo[2]), json_f32(mat.albedo[3]),
            json_f32(mat.roughness), json_f32(mat.metallic),
            emission_part
        ));
        exported_names.push(json_escape(&mesh.name));
        exported_mesh_count += 1;
    }

    // Pad binary buffer to 4-byte alignment
    while bin.len() % 4 != 0 {
        bin.push(0);
    }

    if exported_mesh_count == 0 {
        return Err("No non-empty meshes to export".into());
    }

    // Build nodes (one per exported mesh) and scene
    let nodes_json: Vec<String> = exported_names.iter().enumerate()
        .map(|(i, name)| format!(r#"{{"name":"{}","mesh":{}}}"#, name, i))
        .collect();
    let node_indices: Vec<String> = (0..exported_mesh_count).map(|i| i.to_string()).collect();

    // Assemble glTF JSON
    let json_str = format!(
        r#"{{"asset":{{"version":"2.0","generator":"Azra"}},"scene":0,"scenes":[{{"nodes":[{}]}}],"nodes":[{}],"meshes":[{}],"materials":[{}],"accessors":[{}],"bufferViews":[{}],"buffers":[{{"byteLength":{}}}]}}"#,
        node_indices.join(","),
        nodes_json.join(","),
        mesh_json_parts.join(","),
        material_json_parts.join(","),
        accessors.join(","),
        buffer_views.join(","),
        bin.len()
    );

    // Pad JSON to 4-byte alignment with spaces
    let mut json_bytes = json_str.into_bytes();
    while json_bytes.len() % 4 != 0 {
        json_bytes.push(b' ');
    }

    // Write .glb
    let total_len = 12 + 8 + json_bytes.len() + 8 + bin.len();
    if total_len > u32::MAX as usize {
        return Err("Scene too large for GLB format".into());
    }
    let mut file = std::fs::File::create(path)
        .map_err(|e| format!("Failed to create {}: {}", path, e))?;

    // Header
    file.write_all(b"glTF").map_err(|e| e.to_string())?;                    // magic
    file.write_all(&2u32.to_le_bytes()).map_err(|e| e.to_string())?;        // version
    file.write_all(&(total_len as u32).to_le_bytes()).map_err(|e| e.to_string())?; // total length

    // JSON chunk
    file.write_all(&(json_bytes.len() as u32).to_le_bytes()).map_err(|e| e.to_string())?;
    file.write_all(&0x4E4F534Au32.to_le_bytes()).map_err(|e| e.to_string())?; // "JSON"
    file.write_all(&json_bytes).map_err(|e| e.to_string())?;

    // BIN chunk
    file.write_all(&(bin.len() as u32).to_le_bytes()).map_err(|e| e.to_string())?;
    file.write_all(&0x004E4942u32.to_le_bytes()).map_err(|e| e.to_string())?; // "BIN\0"
    file.write_all(&bin).map_err(|e| e.to_string())?;

    Ok((total_verts, total_faces))
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Sanitize f32 for JSON: NaN/Inf are not valid JSON numbers.
#[inline]
fn json_f32(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

// ============================================================
// Convenience: export from HalfEdgeMesh
// ============================================================

/// Export a HalfEdgeMesh as .glb with default material.
pub fn export_half_edge_mesh_glb(
    path: &str,
    name: &str,
    positions: &[Vec3],
    normals: &[Vec3],
    indices: &[u32],
) -> Result<(usize, usize), String> {
    let uvs = vec![[0.0f32, 0.0]; positions.len()];
    let mesh = ExportMesh {
        name: name.to_string(),
        positions: positions.to_vec(),
        normals: normals.to_vec(),
        uvs,
        indices: indices.to_vec(),
        material: ExportMaterial::default(),
    };
    export_glb(path, &[mesh])
}
