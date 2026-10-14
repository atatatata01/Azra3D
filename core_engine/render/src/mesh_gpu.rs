use bytemuck::{Pod, Zeroable};

// ============================================================
// GPU Vertex Format
// ============================================================

/// GPU-ready vertex for rasterization. 32 bytes, tightly packed.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct RasterVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

impl RasterVertex {
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Self>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            // location 0: position vec3
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            // location 1: normal vec3
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 12,
                shader_location: 1,
            },
            // location 2: uv vec2
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 24,
                shader_location: 2,
            },
        ],
    };
}

// ============================================================
// GPU Mesh (uploaded vertex + index buffers)
// ============================================================

/// A mesh uploaded to GPU memory, ready for draw calls.
///
/// Buffers are CAPACITY-backed with `COPY_DST` so an editable mesh (voxel sculpt updates
/// ~60×/s) can be rewritten in place via `update_raw` → `queue.write_buffer` instead of
/// recreating the buffers every frame. Recreating ~9 MB of buffers per frame
/// (`create_buffer_init`) was the per-frame stall that capped interactive poly count.
pub struct GpuMesh {
    pub vertex_buffer: wgpu::Buffer,
    pub index_buffer: wgpu::Buffer,
    pub index_count: u32,
    pub index_format: wgpu::IndexFormat,
    /// Allocated buffer sizes in bytes (>= current data). `update_raw` only grows (recreates)
    /// when the new data exceeds these; otherwise it writes in place.
    pub vertex_capacity: u64,
    pub index_capacity: u64,
}

impl GpuMesh {
    /// Upload vertex and index data to the GPU (also `COPY_DST` so it can be updated in place).
    pub fn upload(
        device: &wgpu::Device,
        vertices: &[RasterVertex],
        indices: &[u32],
        label: Option<&str>,
    ) -> Self {
        use wgpu::util::DeviceExt;
        let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: label.map(|l| format!("{l}_vb")).as_deref(),
            contents: bytemuck::cast_slice(vertices),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });
        let ib = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: label.map(|l| format!("{l}_ib")).as_deref(),
            contents: bytemuck::cast_slice(indices),
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        });
        let (vcap, icap) = (vb.size(), ib.size());
        Self {
            vertex_buffer: vb,
            index_buffer: ib,
            index_count: indices.len() as u32,
            index_format: wgpu::IndexFormat::Uint32,
            vertex_capacity: vcap,
            index_capacity: icap,
        }
    }

    /// Create from raw byte slices with headroom + `COPY_DST` (used by the mesh cache).
    /// Over-allocates to a power-of-two so growth (sculpting adds vertices) rarely reallocates.
    pub fn upload_raw(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertex_bytes: &[u8],
        index_bytes: &[u8],
        index_count: u32,
    ) -> Self {
        let vcap = (vertex_bytes.len() as u64).max(1024).next_power_of_two();
        let icap = (index_bytes.len() as u64).max(1024).next_power_of_two();
        let vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mesh_cache_vb"), size: vcap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
        });
        let ib = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mesh_cache_ib"), size: icap,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
        });
        if !vertex_bytes.is_empty() { queue.write_buffer(&vb, 0, vertex_bytes); }
        if !index_bytes.is_empty() { queue.write_buffer(&ib, 0, index_bytes); }
        Self {
            vertex_buffer: vb, index_buffer: ib, index_count,
            index_format: wgpu::IndexFormat::Uint32,
            vertex_capacity: vcap, index_capacity: icap,
        }
    }

    /// Rewrite the mesh in place via `queue.write_buffer`; recreate (grow to next power-of-two)
    /// ONLY when the new data exceeds the current capacity. This is the editable-mesh hot path:
    /// it replaces the per-frame `create_buffer_init` churn with a cheap buffer write. The stale
    /// tail beyond `index_count` is never drawn, so shrinking is free.
    pub fn update_raw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertex_bytes: &[u8],
        index_bytes: &[u8],
        index_count: u32,
    ) {
        // Trust the LIVE buffer size, not only the capacity watermark — a desync
        // (capacity large, buffer still a 4 B placeholder) caused PathTrace mesh
        // landings to validation-fail and kill the device.
        let v_need = vertex_bytes.len() as u64;
        let i_need = index_bytes.len() as u64;
        let v_ok = v_need <= self.vertex_capacity && v_need <= self.vertex_buffer.size();
        if v_ok {
            if !vertex_bytes.is_empty() {
                queue.write_buffer(&self.vertex_buffer, 0, vertex_bytes);
            }
        } else {
            // Grow with 2× headroom so a steadily-growing mesh (adding material) crosses the
            // capacity boundary far less often — each grow recreates the buffer (a stroke hitch).
            self.vertex_capacity = (v_need.max(1) * 2).next_power_of_two().max(1024);
            self.vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mesh_cache_vb"), size: self.vertex_capacity,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
            });
            if !vertex_bytes.is_empty() {
                queue.write_buffer(&self.vertex_buffer, 0, vertex_bytes);
            }
        }
        let i_ok = i_need <= self.index_capacity && i_need <= self.index_buffer.size();
        if i_ok {
            if !index_bytes.is_empty() {
                queue.write_buffer(&self.index_buffer, 0, index_bytes);
            }
        } else {
            self.index_capacity = (i_need.max(1) * 2).next_power_of_two().max(1024);
            self.index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mesh_cache_ib"), size: self.index_capacity,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
            });
            if !index_bytes.is_empty() {
                queue.write_buffer(&self.index_buffer, 0, index_bytes);
            }
        }
        self.index_count = index_count;
    }

    /// Position-tier partial GPU upload: rewrite **vertex** buffer only (indices unchanged).
    ///
    /// Used when topology is stable (G/R/S, extrude drag slide, shrink/fatten) so the index
    /// buffer and `index_count` stay valid. Falls back to full recreate if the VB lacks capacity.
    /// Byte layout must match the live mesh (same vertex stride / order as last full upload).
    pub fn update_vertices_only(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertex_bytes: &[u8],
    ) {
        let v_need = vertex_bytes.len() as u64;
        let v_ok = v_need <= self.vertex_capacity && v_need <= self.vertex_buffer.size();
        if v_ok {
            if !vertex_bytes.is_empty() {
                queue.write_buffer(&self.vertex_buffer, 0, vertex_bytes);
            }
        } else {
            self.vertex_capacity = (v_need.max(1) * 2).next_power_of_two().max(1024);
            self.vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mesh_cache_vb"),
                size: self.vertex_capacity,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            if !vertex_bytes.is_empty() {
                queue.write_buffer(&self.vertex_buffer, 0, vertex_bytes);
            }
        }
    }

    /// Patch a **byte range** of the vertex buffer (true sub-region upload).
    /// `byte_offset` must be a multiple of 4 (wgpu requirement). No-op if out of capacity —
    /// caller must full-upload instead.
    pub fn patch_vertex_bytes(
        &mut self,
        queue: &wgpu::Queue,
        byte_offset: u64,
        vertex_bytes: &[u8],
    ) -> bool {
        let end = byte_offset + vertex_bytes.len() as u64;
        if end > self.vertex_capacity || end > self.vertex_buffer.size() {
            return false;
        }
        if !vertex_bytes.is_empty() {
            queue.write_buffer(&self.vertex_buffer, byte_offset, vertex_bytes);
        }
        true
    }

    /// DirtyMeshRegion upload: write only **changed** vertex slots vs `prev`.
    /// Scans for contiguous runs of differing verts and issues one `write_buffer` per run
    /// (plus a full write if >25% of verts changed — avoids run explosion).
    /// `prev` and `new` must be the same length (topology-stable position tier).
    pub fn update_vertices_dirty_runs(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        prev: &[[f32; 8]],
        new: &[[f32; 8]],
    ) {
        if prev.len() != new.len() || new.is_empty() {
            let bytes: &[u8] = bytemuck::cast_slice(new);
            self.update_vertices_only(device, queue, bytes);
            return;
        }
        let n = new.len();
        let mut changed = 0usize;
        let mut runs: Vec<(usize, usize)> = Vec::new(); // inclusive start, exclusive end
        let mut run_start: Option<usize> = None;
        for i in 0..n {
            let diff = prev[i] != new[i];
            if diff {
                changed += 1;
                if run_start.is_none() {
                    run_start = Some(i);
                }
            } else if let Some(s) = run_start.take() {
                runs.push((s, i));
            }
        }
        if let Some(s) = run_start {
            runs.push((s, n));
        }
        // Full rewrite when mostly dirty or too many runs (driver call overhead).
        if changed * 4 > n || runs.len() > 64 {
            let bytes: &[u8] = bytemuck::cast_slice(new);
            self.update_vertices_only(device, queue, bytes);
            return;
        }
        if runs.is_empty() {
            return;
        }
        let v_need = (n * std::mem::size_of::<[f32; 8]>()) as u64;
        if v_need > self.vertex_capacity || v_need > self.vertex_buffer.size() {
            let bytes: &[u8] = bytemuck::cast_slice(new);
            self.update_vertices_only(device, queue, bytes);
            return;
        }
        let stride = std::mem::size_of::<[f32; 8]>() as u64;
        for (s, e) in runs {
            let bytes: &[u8] = bytemuck::cast_slice(&new[s..e]);
            queue.write_buffer(&self.vertex_buffer, s as u64 * stride, bytes);
        }
    }
}

// ============================================================
// Per-Material Uniform
// ============================================================

/// Material data for the rasterized PBR fragment shader. 256 bytes (16 x vec4).
///
/// Includes base PBR + procedural layers (noise, edge wear, cavity dirt)
/// matching the SDF compute path's `eval_material()`, plus texture map handles.
///
/// LAYOUT MUST STAY IN LOCKSTEP with `MaterialData` in `render/src/gbuffer.wgsl`,
/// `shadow.wgsl`, and `PtMaterial` in `pathtrace_tri_entry.wgsl`.
///
/// Size = 20 × vec4 = **320 B** (was 304; +1 lane for the SSS color map slot).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct RasterMaterial {
    /// RGB albedo + opacity in alpha
    pub albedo: [f32; 4],
    /// x=roughness, y=metallic, z=emission_intensity, w=shader_type ordinal (ShaderType::as_u32)
    pub roughness_metallic: [f32; 4],
    /// RGB emission color + 0
    pub emission_color: [f32; 4],
    /// Noise pattern: amount, scale, octaves (as f32), metallic
    pub noise_params: [f32; 4],
    /// Noise pattern: color.rgb, roughness
    pub noise_color: [f32; 4],
    /// Edge wear: amount, roughness, metallic, sharpness
    pub edge_wear: [f32; 4],
    /// Edge wear: color.rgb, 0
    pub edge_wear_color: [f32; 4],
    /// Cavity dirt: amount, roughness, metallic, sharpness
    pub cavity_dirt: [f32; 4],
    /// Cavity dirt: color.rgb, 0
    pub cavity_dirt_color: [f32; 4],
    /// Albedo texture map: x=slot (f32 of u32, 0=none), y=strength, z=uv_scale,
    /// w=projection (0=triplanar, 1=UV). Sampled from the material texture array.
    pub albedo_map: [f32; 4],
    /// Tangent-space normal map (same packing as albedo_map). Perturbs the G-buffer normal
    /// via the whiteout triplanar blend (body-local sampling).
    pub normal_map: [f32; 4],
    /// Roughness map (same packing). Grayscale .r REPLACES scalar roughness by strength.
    pub roughness_map: [f32; 4],
    /// Metallic map (same packing). Grayscale .r REPLACES scalar metallic by strength.
    pub metallic_map: [f32; 4],
    /// Emission map (color). Multiplies emission_color * intensity by sampled rgb.
    pub emission_map: [f32; 4],
    /// Bump / height map (grayscale). Perturbs normal from height derivatives.
    pub bump_map: [f32; 4],
    /// Displacement map (grayscale). Vertex offset along normal by sample * strength.
    pub displacement_map: [f32; 4],
    /// SSS color map (sRGB color, same packing). Sampled rgb REPLACES `adv_sss.xyz`
    /// by `strength` in the G-buffer's subsurface wrap approximation.
    pub sss_color_map: [f32; 4],
    /// Advanced lobes (realtime approx in gbuffer; full transport = path tracer):
    /// x=sss_radius, y=ior, z=clearcoat, w=clearcoat_roughness
    pub adv_params: [f32; 4],
    /// x..z=sss_color, w=sheen
    pub adv_sss: [f32; 4],
    /// x..z=glass_tint, w=sheen_tint
    pub adv_glass: [f32; 4],
}

/// Authored-to-shader remap for the two GRADIENT maps (Normal, Bump).
///
/// Their shader effect is a derivative, not a colour: a screen-space finite-difference slope for
/// Bump and a tangent-space perturbation for Normal. At authored `strength = 1.0` that came out
/// ~10x too strong for ambientCG / typical 0..1 height and normal maps, so the only usable part of
/// the slider was its bottom tenth and a library drag-drop looked crushed.
///
/// The fix is a REMAP, not a smaller default (owner 2026-08-08): authored `1.0` now produces what
/// `0.1` produced before, so the whole 0..1 slider is usable and 1.0 means "full, correct effect".
/// Doing it here — the single point where a `TexMap` becomes GPU floats — covers every consumer
/// (raster G-buffer, shadow, and the path tracer, which reads the same packed `mat.*_map.y` lane)
/// without touching a shader. Colour/scalar maps (albedo, roughness, metallic, emission,
/// displacement) are NOT gradients and keep 1:1.
///
/// NOTE: this rescales previously-saved materials — a stored `1.0` now renders at the old `0.1`.
/// That is the intended correction (owner-chosen), not a migration bug.
pub const GRADIENT_MAP_STRENGTH_REMAP: f32 = 0.1;

/// Bump-specific remap. Bump's screen-space finite-difference slope is harsher than Normal's
/// tangent-space perturbation, so the shared 0.1 still read ~2x too strong at authored `1.0`
/// (owner 2026-08-09). Halved to 0.05 — Normal keeps [`GRADIENT_MAP_STRENGTH_REMAP`].
/// Same deal as above: old saves rescale, which is the intended correction (owner-accepted).
pub const BUMP_MAP_STRENGTH_REMAP: f32 = 0.05;

/// Pack one `TexMap` channel into the shader's vec4 layout (slot bits, strength, tiling, proj bits).
/// Projection word: bit0 = UV/Box, bit1 = absolute sample (no free tint Mult).
///
/// `strength_scale` is the authored→shader remap: [`GRADIENT_MAP_STRENGTH_REMAP`] for the gradient
/// maps (Normal / Bump), `1.0` for everything else.
fn pack_tex_map(m: &core_math::pbr_material::TexMap, strength_scale: f32) -> [f32; 4] {
    [
        f32::from_bits(m.tex_id),
        m.strength * strength_scale,
        m.uv_scale,
        f32::from_bits(m.projection_packed()),
    ]
}

impl RasterMaterial {
    /// Convert from the engine's PbrMaterial, including all procedural layers.
    pub fn from_pbr(mat: &core_math::primitives::PbrMaterial) -> Self {
        let np = &mat.noise_pattern;
        let ew = &mat.edge_wear;
        let cd = &mat.cavity_dirt;
        Self {
            albedo: [mat.albedo.x, mat.albedo.y, mat.albedo.z, mat.opacity],
            roughness_metallic: [
                mat.roughness,
                mat.metallic,
                mat.emission_intensity,
                mat.shader_type.as_u32() as f32,
            ],
            emission_color: [mat.emission_color.x, mat.emission_color.y, mat.emission_color.z, 0.0],
            noise_params: [
                if np.enabled { np.amount } else { 0.0 },
                np.scale,
                np.octaves as f32,
                np.metallic,
            ],
            noise_color: [np.color.x, np.color.y, np.color.z, np.roughness],
            edge_wear: [
                if ew.enabled { ew.amount } else { 0.0 },
                ew.roughness,
                ew.metallic,
                ew.sharpness,
            ],
            edge_wear_color: [ew.color.x, ew.color.y, ew.color.z, 0.0],
            cavity_dirt: [
                if cd.enabled { cd.amount } else { 0.0 },
                cd.roughness,
                cd.metallic,
                cd.sharpness,
            ],
            cavity_dirt_color: [cd.color.x, cd.color.y, cd.color.z, 0.0],
            albedo_map: pack_tex_map(&mat.albedo_map, 1.0),
            // Normal + Bump are GRADIENT maps — remapped (Normal 0.1, Bump 0.05).
            normal_map: pack_tex_map(&mat.normal_map, GRADIENT_MAP_STRENGTH_REMAP),
            roughness_map: pack_tex_map(&mat.roughness_map, 1.0),
            metallic_map: pack_tex_map(&mat.metallic_map, 1.0),
            emission_map: pack_tex_map(&mat.emission_map, 1.0),
            bump_map: pack_tex_map(&mat.bump_map, BUMP_MAP_STRENGTH_REMAP),
            displacement_map: pack_tex_map(&mat.displacement_map, 1.0),
            sss_color_map: pack_tex_map(&mat.sss_color_map, 1.0),
            adv_params: [
                mat.sss_radius,
                mat.ior,
                mat.clearcoat,
                mat.clearcoat_roughness,
            ],
            adv_sss: [
                mat.sss_color.x,
                mat.sss_color.y,
                mat.sss_color.z,
                mat.sheen,
            ],
            adv_glass: [
                mat.glass_tint.x,
                mat.glass_tint.y,
                mat.glass_tint.z,
                mat.sheen_tint,
            ],
        }
    }
}

const _: () = assert!(std::mem::size_of::<RasterMaterial>() == 320);

// ============================================================
// Per-Instance Data (instanced drawing)
// ============================================================

/// Per-draw-call instance data. 144 bytes, aligned for GPU.
///
/// Contains model transform + normal matrix + material index + vertex-curvature shape.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct InstanceData {
    /// 4x4 model-to-world matrix (column-major)
    pub model_matrix: [[f32; 4]; 4],
    /// 3x4 normal matrix (inverse-transpose of upper-left 3x3, padded to 3 vec4s)
    pub normal_matrix: [[f32; 4]; 3],
    /// Index into the material storage buffer
    pub material_idx: u32,
    /// Per-frame body index for silhouette outline (1-based, 0 = no body)
    pub body_index: u32,
    /// Per-instance render flags. Bit 0 = USE_VERTEX_COLOR: the fragment shader reads the vertex's
    /// UV slot as a packed paint colour instead of a texture coordinate (voxel sculpt-vertex-paint).
    /// Off for every other mesh (poly/SDF/NURBS keep their real UVs untouched).
    pub flags: u32,
    pub _pad: u32,
    /// Vertex-curvature shaping (material panel Vertex rung). x=remap_min, y=remap_max,
    /// z=layer opacity 0..1 (**intensity only**), w=Soft/Hard power (≥1 harder, <1 softer;
    /// `pow(mag,w)` keeps endpoints 0/1 — soft does not dim peak white). Identity = [0,1,1,1].
    /// Meaningless unless `INSTANCE_FLAG_VERTEX_CURVATURE` is set.
    pub curv_params: [f32; 4],
}

/// Instance flag bit: interpret the vertex UV slot as a packed paint colour (voxel sculpt paint).
pub const INSTANCE_FLAG_VERTEX_COLOR: u32 = 1;

/// Instance flag bit: this body is the active Move-grab TARGET. The G-buffer VERTEX shader (`vs_main`)
/// displaces its verts by `falloff·drag` read from `CameraUniforms.grab_*`. Set ONLY on the grabbed
/// voxel body's instance during a mesh-grab; 0 everywhere else, so the displacement is a structural
/// no-op for all other geometry. This makes the live grab preview the EXACT mesh that is drawn (the
/// resident pool) — no CPU mesh, no per-chunk render gate (the failure mode of the prior CPU passes).
/// Mirrors Meta Medium's Move tool, which displaces in the sculpt vertex shader then re-voxelizes on
/// release. Independent bit from `INSTANCE_FLAG_VERTEX_COLOR`, so the two compose (paint + grab).
pub const INSTANCE_FLAG_GRAB_TARGET: u32 = 2;

/// Instance flag bit: this SDF body has a resident `sdf_body_curvature` buffer bound at group 2
/// (GPU_CURVATURE_KERNEL_PLAN.md P2 — the material panel's Vertex rung). The G-buffer vertex
/// shader reads `curv_vertex[vertex_index * 4 + CHANNEL]` (see
/// `INSTANCE_FLAG_VERTEX_CURV_CONVEX`/`_CONCAVE` for CHANNEL) ONLY when this bit is set — every
/// other draw (including SDF bodies that have no Vertex-rung generator) reads a 1-element dummy at
/// group 2 and never touches it, so the group-2 bind is always valid but only load-bearing when
/// this flag says so. Independent bit from `INSTANCE_FLAG_VERTEX_COLOR` /
/// `INSTANCE_FLAG_GRAB_TARGET` — composes with both (a body cannot be voxel-sculpt-painted AND
/// SDF-curvature-mapped at the same time in practice, but nothing here forbids it structurally).
pub const INSTANCE_FLAG_VERTEX_CURVATURE: u32 = 4;

/// Instance flag bit: the vertex-curvature carry (bit 2 above) should read the CONVEX-mask channel
/// (`curv_vertex[i*4+2]`) instead of the default signed-curvature channel
/// (`curv_vertex[i*4+1]`). Mutually exclusive with `INSTANCE_FLAG_VERTEX_CURV_CONCAVE` — set by
/// `raster_gbuffer.rs` from the Generator layer's own `GeneratorOutput::ConvexOnly` selection
/// (`CurvVertexSlot::out_channel == 2`). Meaningless unless bit 2 is also set.
pub const INSTANCE_FLAG_VERTEX_CURV_CONVEX: u32 = 8;

/// Instance flag bit: the vertex-curvature carry should read the CONCAVE-mask channel
/// (`curv_vertex[i*4+3]`), for `GeneratorOutput::ConcaveOnly` / the Cavity generator
/// (`CurvVertexSlot::out_channel == 3`). Mutually exclusive with `INSTANCE_FLAG_VERTEX_CURV_CONVEX`.
/// Meaningless unless bit 2 is also set.
pub const INSTANCE_FLAG_VERTEX_CURV_CONCAVE: u32 = 16;

/// Instance flag bit: invert the shaped vertex-curvature mask (panel Inv checkbox).
/// Meaningless unless bit 2 is also set.
pub const INSTANCE_FLAG_VERTEX_CURV_INVERT: u32 = 32;

/// Bits 6..8 of `InstanceData.flags`: layer blend mode for vertex-curvature composite
/// (panel `BlendMode` on the Vertex-rung generator layer). Packed as
/// `(blend as u32) << INSTANCE_FLAG_VERTEX_CURV_BLEND_SHIFT`.
/// Values: 0=Normal, 1=Multiply, 2=Screen, 3=Overlay, 4=Add — matches panel `BlendMode` order.
/// Meaningless unless bit 2 is also set.
pub const INSTANCE_FLAG_VERTEX_CURV_BLEND_SHIFT: u32 = 6;
pub const INSTANCE_FLAG_VERTEX_CURV_BLEND_MASK: u32 = 0x7 << INSTANCE_FLAG_VERTEX_CURV_BLEND_SHIFT;

/// Pack a panel blend mode (0..4) into instance flags.
#[inline]
pub fn pack_vertex_curv_blend(blend: u32) -> u32 {
    (blend.min(4) << INSTANCE_FLAG_VERTEX_CURV_BLEND_SHIFT) & INSTANCE_FLAG_VERTEX_CURV_BLEND_MASK
}

/// Instance flag bit 9: vertex curvature is a **layer mask** (Photoshop-style), not content.
/// White = show the layer (post-map color), black = hide it (revert to pre-map base).
/// Meaningless unless bit 2 is also set.
pub const INSTANCE_FLAG_VERTEX_CURV_AS_MASK: u32 = 1 << 9;

impl InstanceData {
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Self>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &[
            // model_matrix: 4 columns as vec4
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 0,
                shader_location: 3,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 16,
                shader_location: 4,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 32,
                shader_location: 5,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 48,
                shader_location: 6,
            },
            // normal_matrix: 3 rows as vec4
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 64,
                shader_location: 7,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 80,
                shader_location: 8,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 96,
                shader_location: 9,
            },
            // material_idx
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 112,
                shader_location: 10,
            },
            // body_index (for silhouette outline)
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 116,
                shader_location: 11,
            },
            // flags (bit 0 = USE_VERTEX_COLOR)
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 120,
                shader_location: 12,
            },
            // curv_params (remap_min, remap_max, opacity, sharpness) — loc 13
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 128,
                shader_location: 13,
            },
        ],
    };

    /// Build instance data from a glam transform.
    pub fn from_transform(
        position: glam::Vec3,
        rotation_deg: glam::Vec3,
        scale: glam::Vec3,
        material_idx: u32,
        body_index: u32,
    ) -> Self {
        let rot = glam::Quat::from_euler(
            glam::EulerRot::YXZ,
            rotation_deg.y.to_radians(),
            rotation_deg.x.to_radians(),
            rotation_deg.z.to_radians(),
        );
        let model = glam::Mat4::from_scale_rotation_translation(scale, rot, position);
        // Normal matrix = inverse-transpose of upper-left 3x3
        // Guard against singular transforms (near-zero scale) producing NaN
        let normal_mat = if scale.x.abs() < 1e-7 || scale.y.abs() < 1e-7 || scale.z.abs() < 1e-7 {
            glam::Mat3::IDENTITY
        } else {
            glam::Mat3::from_mat4(model).inverse().transpose()
        };

        Self {
            model_matrix: model.to_cols_array_2d(),
            normal_matrix: [
                [normal_mat.x_axis.x, normal_mat.x_axis.y, normal_mat.x_axis.z, 0.0],
                [normal_mat.y_axis.x, normal_mat.y_axis.y, normal_mat.y_axis.z, 0.0],
                [normal_mat.z_axis.x, normal_mat.z_axis.y, normal_mat.z_axis.z, 0.0],
            ],
            material_idx,
            body_index,
            flags: 0,
            _pad: 0,
            curv_params: [0.0, 1.0, 1.0, 1.0],
        }
    }

    /// Set the per-instance render flags (e.g. `INSTANCE_FLAG_VERTEX_COLOR`) and return self, for a
    /// fluent `InstanceData::from_model_matrix(..).with_flags(..)` at the draw-call build site.
    pub fn with_flags(mut self, flags: u32) -> Self { self.flags = flags; self }

    /// Vertex-curvature shaping params (remap_min, remap_max, opacity, sharpness).
    pub fn with_curv_params(mut self, params: [f32; 4]) -> Self {
        self.curv_params = params;
        self
    }

    /// Build instance data directly from a model→world matrix (RP6). Avoids the lossy,
    /// wasteful Mat4 → scale/rot/trans → euler-deg → quat → Mat4 round-trip that callers
    /// otherwise perform on an already-built world transform. Normal matrix uses the same
    /// singularity guard as `from_transform` (scale magnitude, here the column lengths).
    ///
    /// C-scale: pure translation / rigid orthonormal bases skip `Mat3::inverse` (was O(N)
    /// full 3×3 invert per instance at 10k shared forests — stress boxes are TRS with S=1).
    pub fn from_model_matrix(model: glam::Mat4, material_idx: u32, body_index: u32) -> Self {
        let col_x = model.x_axis.truncate();
        let col_y = model.y_axis.truncate();
        let col_z = model.z_axis.truncate();
        let sx = col_x.length();
        let sy = col_y.length();
        let sz = col_z.length();
        let normal_mat = if sx < 1e-7 || sy < 1e-7 || sz < 1e-7 {
            glam::Mat3::IDENTITY
        } else {
            // Orthonormal columns (rigid or pure translate): normal matrix = rotation part.
            // |‖c‖−1| and |cᵢ·cⱼ| tests catch stress-grid instances without inverse.
            let ux = col_x / sx;
            let uy = col_y / sy;
            let uz = col_z / sz;
            let unit = (sx - 1.0).abs() < 1e-3
                && (sy - 1.0).abs() < 1e-3
                && (sz - 1.0).abs() < 1e-3;
            let ortho = ux.dot(uy).abs() < 1e-3
                && ux.dot(uz).abs() < 1e-3
                && uy.dot(uz).abs() < 1e-3;
            if unit && ortho {
                glam::Mat3::from_cols(ux, uy, uz)
            } else if (sx - sy).abs() < 1e-4 && (sx - sz).abs() < 1e-4 {
                // Uniform scale: normal = R / s  ≡  orthonormal axes / s
                glam::Mat3::from_cols(ux / sx, uy / sx, uz / sx)
            } else {
                glam::Mat3::from_mat4(model).inverse().transpose()
            }
        };

        Self {
            model_matrix: model.to_cols_array_2d(),
            normal_matrix: [
                [normal_mat.x_axis.x, normal_mat.x_axis.y, normal_mat.x_axis.z, 0.0],
                [normal_mat.y_axis.x, normal_mat.y_axis.y, normal_mat.y_axis.z, 0.0],
                [normal_mat.z_axis.x, normal_mat.z_axis.y, normal_mat.z_axis.z, 0.0],
            ],
            material_idx,
            body_index,
            flags: 0,
            _pad: 0,
            curv_params: [0.0, 1.0, 1.0, 1.0],
        }
    }
}

const _: () = assert!(std::mem::size_of::<InstanceData>() == 144);
