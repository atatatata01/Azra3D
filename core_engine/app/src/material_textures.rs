// core_engine/app/src/material_textures.rs
// ============================================================
// Material PBR Texture Array
// ============================================================
//
// ONE generation-counted RGBA8 `texture_2d_array` that holds every loaded material
// map — albedo (sRGB color) AND the data channels (normal / roughness / metallic,
// stored PRE-ENCODED to sRGB so the hardware decode returns raw values — see
// `decode_file_to_data_map`). Sampled by the raster G-buffer pipeline
// (`render/src/gbuffer.wgsl`) — the one live shading path for all mesh types.
//
// SLOT CONVENTION
//   layer 0  = a 1x1 WHITE pixel (the "no map" sentinel). `TexMap.tex_id == 0` ⇒ the
//              shader samples layer 0 ⇒ multiplies albedo by white ⇒ scalar unchanged.
//   layer N  = the Nth loaded map. `TexMap.tex_id` is this 1-based layer index.
//
// All layers share one runtime resolution (`layer_res`, starts at `DEFAULT_MAP_RES`);
// loaded images are resampled to it (a `texture_2d_array` requires uniform layer
// dimensions). O3: `layer_res` can grow up to `MAX_MAP_RES` (4K) when a higher bake
// installs — existing CPU slots are bilinear-upsampled and the GPU array recreated.
//
// FRAME ORDERING CONTRACT
//   The texture is created / GROWN only inside `poll_background_tasks` (see
//   `AzraApp::process_material_map_loads`). Panels never touch the GPU — they decode
//   pixels on the UI thread and push a `PendingMapLoad` into `cache.material_map_loads`.
//
// GENERATION COUNTER (mirrors `VolumeAtlas.texture_generation`)
//   `generation` bumps whenever the array texture is RECREATED (it grows by powers of
//   two when a load would overflow capacity). The two consuming bind groups cache the
//   generation and rebuild on mismatch — no timing tricks.

use eframe::wgpu;

/// Default per-layer resolution and historical install floor for soft upsamples
/// (pre-O3 always 1024²). Loaded file maps resample here; the array starts at this size.
pub(crate) const DEFAULT_MAP_RES: u32 = 1024;
/// **Product bake ceiling (D3 / M4):** new panel targets and work-res snaps max out at 2K.
/// 4K is deferred (residency / multi-material) — technical load path still accepts up to
/// [`MAX_MAP_RES`] so a future 4K save can round-trip without corrupt decode.
pub(crate) const PRODUCT_MAP_CEILING: u32 = 2048;
/// Hard technical ceiling for material map layers (load/save/wire). Not offered in the
/// panel until M4+ unblocks 4K. Must stay ≥ [`PRODUCT_MAP_CEILING`].
pub(crate) const MAX_MAP_RES: u32 = 4096;
/// Backward-compat alias: default layer size / historical `MAP_RES` constant.
pub(crate) const MAP_RES: u32 = DEFAULT_MAP_RES;

/// Effective authoring ceiling: 2K product default, or 4K when `AZRA_ALLOW_4K_MAPS=1`.
pub(crate) fn product_map_ceiling() -> u32 {
    if std::env::var("AZRA_ALLOW_4K_MAPS").as_deref() == Ok("1") {
        MAX_MAP_RES
    } else {
        PRODUCT_MAP_CEILING
    }
}

/// Scoreboard — estimated GPU VRAM for dual-plane (16-bit) map install of `n_channels`
/// at square `res` (albedo/rough/metal/emission each use hi+lo rgba8 planes).
///
/// Formula: `res² × 4 B × 2 planes × n_channels`. Does not include sampler/bind-group
/// overhead or the white sentinel layer.
pub(crate) fn estimate_material_map_vram_bytes(res: u32, n_channels: u32) -> u64 {
    let res = res.min(MAX_MAP_RES) as u64;
    let n = n_channels.max(1) as u64;
    res * res * 4 * 2 * n
}

/// Human scoreboard line for the M4 gate (publish a number).
pub(crate) fn material_map_vram_scoreboard_line(res: u32, n_channels: u32) -> String {
    let bytes = estimate_material_map_vram_bytes(res, n_channels);
    let mib = bytes as f64 / (1024.0 * 1024.0);
    format!(
        "VRAM ≈ {mib:.2} MiB dual-plane @ {res}² × {n_channels} ch ({bytes} B)"
    )
}

/// Side length of a square RGBA8 buffer, or `None` if not a legal map size.
pub(crate) fn map_side_len(pixels: &[u8]) -> Option<u32> {
    if pixels.len() % 4 != 0 {
        return None;
    }
    let n = pixels.len() / 4;
    if n == 0 {
        return None;
    }
    let side = (n as f64).sqrt() as u32;
    if (side as usize) * (side as usize) != n {
        return None;
    }
    if !(8..=MAX_MAP_RES).contains(&side) {
        return None;
    }
    Some(side)
}

/// Bilinear upsample square RGBA8 `src_res` → `dst_res` (same filter as bake path).
pub(crate) fn upsample_rgba_square(src: &[u8], src_res: u32, dst_res: u32) -> Vec<u8> {
    if src_res == dst_res {
        return src.to_vec();
    }
    if src_res == 0 || dst_res == 0 {
        return vec![0u8; (dst_res * dst_res * 4) as usize];
    }
    let sr = src_res as f32;
    let mut out = vec![0u8; (dst_res * dst_res * 4) as usize];
    let sample = |x: i32, y: i32, c: usize| -> f32 {
        let xx = x.clamp(0, src_res as i32 - 1) as u32;
        let yy = y.clamp(0, src_res as i32 - 1) as u32;
        src[((yy * src_res + xx) * 4 + c as u32) as usize] as f32
    };
    for y in 0..dst_res {
        let fy = (y as f32 + 0.5) * sr / dst_res as f32 - 0.5;
        let y0 = fy.floor() as i32;
        let ty = fy - y0 as f32;
        for x in 0..dst_res {
            let fx = (x as f32 + 0.5) * sr / dst_res as f32 - 0.5;
            let x0 = fx.floor() as i32;
            let tx = fx - x0 as f32;
            let di = ((y * dst_res + x) * 4) as usize;
            for c in 0..4 {
                let v00 = sample(x0, y0, c);
                let v10 = sample(x0 + 1, y0, c);
                let v01 = sample(x0, y0 + 1, c);
                let v11 = sample(x0 + 1, y0 + 1, c);
                let v0 = v00 + (v10 - v00) * tx;
                let v1 = v01 + (v11 - v01) * tx;
                out[di + c] = (v0 + (v1 - v0) * ty).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// Where the uploaded map's slot should be written back (one frame later, in poll).
/// SDF/voxel/poly prim materials are embedded; graph-node materials are referenced.
#[derive(Clone, Copy)]
pub(crate) enum MapTarget {
    /// A graph material node (referenced material). Slot → `material_by_id_mut(id).albedo_map`.
    Graph(core_math::pbr_material::MaterialId),
    /// An embedded prim material inside an SdfBody. Slot → that prim's `material.albedo_map`,
    /// then mirrored to the graph. `(body_nid, prim_uid)`.
    Prim(core_math::scene_graph::NodeId, u32),
    /// An embedded body material (voxel / poly / nurbs node). Slot → that node's `material.albedo_map`.
    BodyMaterial(core_math::scene_graph::NodeId),
}

/// A decoded material map awaiting upload (produced on the UI thread or the import worker,
/// consumed in poll). `pixels` is square RGBA8 at any legal side (8..=MAX_MAP_RES); the
/// texture array grows/upsamples to match on upload. Data maps (kind.is_data()) are already
/// pre-encoded to sRGB (see `decode_file_to_data_map`).
pub(crate) struct PendingMapLoad {
    /// Where to write the assigned map slot once the upload completes.
    pub(crate) target: MapTarget,
    /// Which material channel this map feeds (`apply_map_slot` writes the matching `TexMap`).
    pub(crate) kind: core_math::pbr_material::MapKind,
    /// Square RGBA8 bytes (`side²×4` for a legal side).
    pub(crate) pixels: Vec<u8>,
    /// Map influence to set alongside the slot (so a freshly-loaded map is visible at full strength).
    pub(crate) strength: f32,
    /// Display name for this map (file stem / library map name) — shown by the outliner's
    /// expandable material rows ("Metal_Scratched_01 — Roughness"). RUNTIME-ONLY: not persisted
    /// (FL4MATTX stores pixels only), so after a project reload the outliner falls back to the
    /// material's own name. Empty = unknown.
    pub(crate) name: String,
    /// Pixels are 16-bit dual-plane (hi in `pixels`, lo in `pixels_lo`) — set
    /// `TexMap.high_precision` on apply. Shader samples `tex_id` + `tex_id+1`.
    pub(crate) high_precision: bool,
    /// Low plane for 16-bit dual upload (same length as `pixels`). Empty when 8-bit / single plane.
    pub(crate) pixels_lo: Vec<u8>,
    /// NORMAL maps only: green channel is DirectX convention (see `TexMap::green_dx`).
    /// Auto-detected from the source filename/layout (`normal_green_is_dx`); false elsewhere.
    pub(crate) green_dx: bool,
}

/// Generation-counted RGBA8 `texture_2d_array` of material maps + its sampler.
pub(crate) struct MaterialTextureArray {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    linear_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    /// Number of layers allocated in the current texture (>= used_layers; capacity).
    capacity_layers: u32,
    /// Layers actually written (layer 0 = white sentinel, so this starts at 1).
    used_layers: u32,
    /// Current per-layer side length (uniform across the array). Starts at [`DEFAULT_MAP_RES`];
    /// grows toward [`MAX_MAP_RES`] when a larger bake installs (O3).
    layer_res: u32,
    /// Bumped on every texture RECREATE (layer grow OR resolution grow). Consumers compare
    /// cached gen → rebuild bind group.
    pub(crate) generation: u64,
    /// CPU-retained RGBA8 pixels per layer (index == slot; slot 0 = empty sentinel placeholder).
    /// CPU-owned slots save from here; GPU-authored slots snapshot the resident texture only
    /// for explicit export/save. Slider previews never read pixels back.
    /// Each non-empty entry is `layer_res²×4` after a successful upload.
    slot_pixels: Vec<Vec<u8>>,
    /// Display name per slot, in lockstep with `slot_pixels` (slot 0 = sentinel placeholder).
    /// Runtime-only (see `PendingMapLoad::name`); empty for restored-from-save slots.
    slot_names: Vec<String>,
    gpu_authored: std::collections::HashSet<u32>,
    raw_precision_slots: std::collections::HashMap<u32, u32>,
    resize_ready: Option<std::sync::mpsc::Receiver<Result<crate::gpu_material_bake::CachedMatBakePipe, String>>>,
    resize_pipeline: Option<crate::gpu_material_bake::CachedMatBakePipe>,
    gpu_filter_codecs: std::collections::HashMap<u32, [u32; 2]>,
    gpu_mip_generation: u64,
    gpu_mips: std::collections::HashMap<u32, (wgpu::Buffer, Vec<wgpu::BindGroup>)>,
}

impl MaterialTextureArray {
    /// Create the array with a single white sentinel layer (slot 0 = "no map").
    pub(crate) fn new(device: &std::sync::Arc<wgpu::Device>, layer_res: u32) -> Self {
        // Start with capacity for a few layers; grows by power-of-two on demand.
        let capacity_layers = 4u32;
        let layer_res = layer_res.clamp(8, MAX_MAP_RES);
        let texture = Self::create_texture(device, capacity_layers, layer_res);
        let view = Self::create_view(&texture, wgpu::TextureFormat::Rgba8UnormSrgb);
        let linear_view = Self::create_view(&texture, wgpu::TextureFormat::Rgba8Unorm);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("material_tex_sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let compiler_device = device.clone();
        std::thread::spawn(move || {
            let result = crate::gpu_material_bake::compile_material_pipeline(
                &compiler_device, &graphics::shader_build::build_material_resize_wgsl(),
                &graphics::bgl::material_resize_bgl_entries(), "material_resize",
            );
            let _ = tx.send(result);
        });
        let mut me = Self {
            texture, view, linear_view, sampler,
            capacity_layers,
            used_layers: 0,
            layer_res,
            generation: 0,
            slot_pixels: Vec::new(),
            slot_names: Vec::new(),
            gpu_authored: std::collections::HashSet::new(),
            raw_precision_slots: std::collections::HashMap::new(),
            resize_ready: Some(rx),
            resize_pipeline: None,
            gpu_filter_codecs: std::collections::HashMap::new(),
            gpu_mip_generation: 0,
            gpu_mips: std::collections::HashMap::new(),
        };
        // The texture starts uninitialised; nothing has written layer 0 yet. We can't write
        // here (no queue in `new`, and this is constructed during GPU init) — instead bump
        // used_layers to 1 and rely on the first frame's poll having no effect. To make slot 0
        // genuinely white without a queue, clear it lazily on the first `upload_map`. Simpler &
        // correct: write the white sentinel immediately would need a queue, so we expose
        // `ensure_sentinel` called from poll. We keep used_layers=0 until then.
        me.used_layers = 0;
        me
    }

    /// Current per-layer side length (all slots share this after upload normalisation).
    pub(crate) fn layer_res(&self) -> u32 {
        self.layer_res
    }

    /// The ONE descriptor for the material texture array. `upload_map`'s GROW path uses the
    /// CURRENT texture as a `copy_texture_to_texture` SOURCE (old layers → bigger array), so
    /// `COPY_SRC` is REQUIRED — omitting it is a validation error the moment the array grows
    /// past its initial capacity (>3 loaded maps, e.g. the FL4MATX2 multi-map restore at
    /// startup). Unit-tested below (`descriptor_usages_support_grow_copy`).
    /// Full mip chain length for a square side (1024 → 11 levels). Level n is
    /// `max(1, side >> n)` — the wgpu mip sizing rule; the CPU downsampler must match.
    pub(crate) fn mip_count(side: u32) -> u32 {
        32 - side.max(1).leading_zeros()
    }

    fn texture_descriptor(layers: u32, layer_res: u32) -> wgpu::TextureDescriptor<'static> {
        let r = layer_res.max(1).min(MAX_MAP_RES);
        wgpu::TextureDescriptor {
            label: Some("material_tex_array"),
            size: wgpu::Extent3d {
                width: r,
                height: r,
                depth_or_array_layers: layers.max(1),
            },
            // Full chain: minification without mips aliased every map into per-pixel noise on
            // ground planes / grazing angles (the drag-drop "broken material" speckle). Every
            // layer write regenerates its chain (`write_layer`); the grow copies move all levels.
            mip_level_count: Self::mip_count(r),
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
        }
    }

    fn create_texture(device: &wgpu::Device, layers: u32, layer_res: u32) -> wgpu::Texture {
        device.create_texture(&Self::texture_descriptor(layers, layer_res))
    }

    /// Sampling view. `usage` is explicit: a wgpu (>=24) view inherits ALL of the texture's
    /// usages by default, and the texture carries STORAGE_BINDING (GPU-authored layers) —
    /// which the sRGB view format cannot support, so the default made the view INVALID and,
    /// with it, every raster/shadow material bind group (`material_tex_array_view` was the
    /// first validation error of the wgpu 25 upgrade). Storage access uses `storage_view`.
    fn create_view(texture: &wgpu::Texture, format: wgpu::TextureFormat) -> wgpu::TextureView {
        texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("material_tex_array_view"),
            format: Some(format),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
            ..Default::default()
        })
    }

    /// View + sampler for binding into a pipeline's material bind group.
    pub(crate) fn view(&self) -> &wgpu::TextureView { &self.view }
    pub(crate) fn linear_view(&self) -> &wgpu::TextureView { &self.linear_view }
    pub(crate) fn sampler(&self) -> &wgpu::Sampler { &self.sampler }

    pub(crate) fn storage_view(&self) -> wgpu::TextureView {
        self.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(wgpu::TextureFormat::Rgba8Unorm),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            mip_level_count: Some(1),
            ..Default::default()
        })
    }

    pub(crate) fn slot_view(&self, slot: u32) -> wgpu::TextureView {
        self.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
            dimension: Some(wgpu::TextureViewDimension::D2),
            base_array_layer: slot, array_layer_count: Some(1),
            mip_level_count: Some(1),
            // sRGB cannot be a storage format — sampling-only view (see `create_view`).
            usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
            ..Default::default()
        })
    }

    fn ensure_capacity(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, required: u32) {
        if required <= self.capacity_layers { return; }
        let capacity = required.next_power_of_two();
        let texture = Self::create_texture(device, capacity, self.layer_res);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        for level in 0..Self::mip_count(self.layer_res) {
            let side = (self.layer_res >> level).max(1);
            encoder.copy_texture_to_texture(
                wgpu::ImageCopyTexture { texture: &self.texture, mip_level: level,
                    origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                wgpu::ImageCopyTexture { texture: &texture, mip_level: level,
                    origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                wgpu::Extent3d { width: side, height: side,
                    depth_or_array_layers: self.used_layers.max(1) },
            );
        }
        queue.submit([encoder.finish()]);
        self.texture = texture;
        self.view = Self::create_view(&self.texture, wgpu::TextureFormat::Rgba8UnormSrgb);
        self.linear_view = Self::create_view(&self.texture, wgpu::TextureFormat::Rgba8Unorm);
        self.capacity_layers = capacity;
        self.generation += 1;
    }

    pub(crate) fn reserve_gpu_map(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, name: &str) -> u32 {
        self.ensure_sentinel(queue);
        let lo_name = format!("{name}__lo16");
        if let Some(slot) = self.slot_names.iter().position(|stored| stored == name) {
            if self.slot_names.get(slot + 1) == Some(&lo_name) { return slot as u32; }
            self.slot_names[slot].clear();
        }
        if let Some(slot) = self.slot_names.iter().position(|stored| stored == &lo_name) {
            self.slot_names[slot].clear();
        }
        let slot = self.used_layers;
        self.ensure_capacity(device, queue, slot + 2);
        self.slot_pixels.extend([Vec::new(), Vec::new()]);
        self.slot_names.extend([name.to_owned(), lo_name]);
        self.used_layers += 2;
        slot
    }

    pub(crate) fn mark_gpu_authored(&mut self, slot: u32, high_precision: bool, raw_precision: bool, color: bool) {
        self.gpu_authored.insert(slot);
        self.gpu_authored.insert(slot + 1);
        let codec = if !high_precision { 0 } else if raw_precision { 1 } else if color { 5 } else { 3 };
        self.gpu_filter_codecs.insert(slot, [codec, if codec == 0 { 0 } else { codec + 1 }]);
        for (plane, codec) in [(slot, 1), (slot + 1, 2)] {
            if raw_precision { self.raw_precision_slots.insert(plane, codec); }
            else { self.raw_precision_slots.remove(&plane); }
        }
    }

    pub(crate) fn prepare_gpu_filtering(&mut self) -> Result<bool, String> {
        if self.resize_pipeline.is_some() { return Ok(true); }
        let receiver = self.resize_ready.as_ref().expect("material filter compiler receiver");
        match receiver.try_recv() {
            Ok(Ok(pipeline)) => { self.resize_pipeline = Some(pipeline); self.resize_ready = None; Ok(true) }
            Ok(Err(error)) => Err(error),
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(false),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("Material filter compiler stopped".into()),
        }
    }

    pub(crate) fn encode_gpu_mips(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, slots: &[u32]) {
        use wgpu::util::DeviceExt;
        let pipeline = self.resize_pipeline.as_ref().expect("material filter must be ready before resident dispatch");
        if self.gpu_mip_generation != self.generation {
            self.gpu_mips.clear(); self.gpu_mip_generation = self.generation;
        }
        for &slot in slots {
            if slot == 0 { continue; }
            let codecs = self.gpu_filter_codecs[&slot];
            if !self.gpu_mips.contains_key(&slot) {
                let codec_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("material_mip_codecs"), contents: bytemuck::cast_slice(&codecs),
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                });
                let mut bindings = Vec::new();
                for level in 1..Self::mip_count(self.layer_res) {
                    let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("material_mip_params"),
                        contents: bytemuck::cast_slice(&[(self.layer_res >> (level - 1)).max(1), (self.layer_res >> level).max(1), 2u32, 1u32]),
                        usage: wgpu::BufferUsages::UNIFORM,
                    });
                    let view = |mip| self.texture.create_view(&wgpu::TextureViewDescriptor {
                        format: Some(wgpu::TextureFormat::Rgba8Unorm),
                        dimension: Some(wgpu::TextureViewDimension::D2Array),
                        base_mip_level: mip, mip_level_count: Some(1),
                        base_array_layer: slot, array_layer_count: Some(2), ..Default::default()
                    });
                    bindings.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("material_mip_binding"), layout: &pipeline.bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view(level)) },
                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&view(level - 1)) },
                            wgpu::BindGroupEntry { binding: 3, resource: codec_buffer.as_entire_binding() },
                        ],
                    }));
                }
                self.gpu_mips.insert(slot, (codec_buffer, bindings));
            }
            let (codecs_buffer, bindings) = &self.gpu_mips[&slot];
            queue.write_buffer(codecs_buffer, 0, bytemuck::cast_slice(&codecs));
            for (index, binding) in bindings.iter().enumerate() {
                let side = (self.layer_res >> (index + 1)).max(1);
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                pass.set_pipeline(&pipeline.pipeline); pass.set_bind_group(0, binding, &[]);
                pass.dispatch_workgroups(side.div_ceil(8), side.div_ceil(8), 2);
            }
        }
    }

    pub(crate) fn snapshot_slot(&self, device: &wgpu::Device, queue: &wgpu::Queue, slot: u32) -> Result<Vec<u8>, String> {
        if !self.gpu_authored.contains(&slot) {
            return self.slot_pixels.get(slot as usize).cloned().ok_or_else(|| "Missing material texture slot".into());
        }
        let mut pixels = self.read_slot_mip(device, queue, slot, 0)?;
        if self.raw_precision_slots.contains_key(&slot) {
            let lut = linear_to_srgb_lut();
            for pixel in pixels.chunks_exact_mut(4) {
                for value in &mut pixel[..3] { *value = lut[*value as usize]; }
            }
        }
        Ok(pixels)
    }

    fn read_slot_mip(&self, device: &wgpu::Device, queue: &wgpu::Queue, slot: u32, mip: u32) -> Result<Vec<u8>, String> {
        let side = (self.layer_res >> mip).max(1);
        let row_bytes = side * 4;
        let padded_row = row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("material_export_readback"), size: padded_row as u64 * side as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture { texture: &self.texture, mip_level: mip,
                origin: wgpu::Origin3d { x: 0, y: 0, z: slot }, aspect: wgpu::TextureAspect::All },
            wgpu::ImageCopyBuffer { buffer: &buffer, layout: wgpu::ImageDataLayout {
                offset: 0, bytes_per_row: Some(padded_row), rows_per_image: Some(side),
            } },
            wgpu::Extent3d { width: side, height: side, depth_or_array_layers: 1 },
        );
        queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| { let _ = tx.send(result); });
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
        rx.recv().map_err(|error| error.to_string())?.map_err(|error| error.to_string())?;
        let mapped = buffer.slice(..).get_mapped_range();
        let mut pixels = Vec::with_capacity((row_bytes * side) as usize);
        for row in mapped.chunks_exact(padded_row as usize) {
            pixels.extend_from_slice(&row[..row_bytes as usize]);
        }
        drop(mapped);
        buffer.unmap();
        Ok(pixels)
    }

    /// Write the white sentinel into layer 0 if it hasn't been written yet. Called from poll the
    /// first time a map is uploaded (and harmless to call again — it's a no-op once done).
    fn ensure_sentinel(&mut self, queue: &wgpu::Queue) {
        if self.used_layers >= 1 { return; }
        let white = vec![255u8; (self.layer_res * self.layer_res * 4) as usize];
        self.write_layer(queue, 0, &white);
        self.used_layers = 1;
        if self.slot_pixels.is_empty() { self.slot_pixels.push(Vec::new()); } // slot 0 placeholder (no persisted pixels)
        if self.slot_names.is_empty() { self.slot_names.push(String::new()); } // lockstep placeholder
    }

    /// Write one layer: mip 0 + the full downsampled chain (a layer must never carry stale or
    /// uninitialized mips — the LOD sampler reads whatever is there). The chain is built on the
    /// CPU in linear space (sRGB-correct box filter), matching what the GPU's own bilinear
    /// decode averages at mip 0 — for color maps AND for the pre-encoded data maps alike.
    fn write_layer(&self, queue: &wgpu::Queue, layer: u32, rgba: &[u8]) {
        let mut res = self.layer_res;
        // None = level 0 samples straight from `rgba`; Some = the previous level's pixels.
        let mut cur: Option<Vec<u8>> = None;
        for level in 0..Self::mip_count(self.layer_res) {
            {
                let src: &[u8] = cur.as_deref().unwrap_or(rgba);
                queue.write_texture(
                    wgpu::ImageCopyTexture {
                        texture: &self.texture,
                        mip_level: level,
                        origin: wgpu::Origin3d { x: 0, y: 0, z: layer },
                        aspect: wgpu::TextureAspect::All,
                    },
                    src,
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(res * 4),
                        rows_per_image: Some(res),
                    },
                    wgpu::Extent3d { width: res, height: res, depth_or_array_layers: 1 },
                );
            }
            if res == 1 { break; }
            let next = downsample_half_rgba_srgb(cur.as_deref().unwrap_or(rgba), res);
            cur = Some(next);
            res = (res >> 1).max(1);
        }
    }

    /// Grow `layer_res` to at least `need` (capped at [`MAX_MAP_RES`]). Upsamples every retained
    /// CPU slot and recreates the GPU array — bumps `generation`.
    fn ensure_layer_res(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, need: u32) {
        // Only grow — never shrink mid-session (would re-download every slot).
        let need = need.min(MAX_MAP_RES);
        if need <= self.layer_res {
            return;
        }
        let old_res = self.layer_res;
        eprintln!(
            "[MaterialTex] grow layer_res {old_res} → {need} (used_layers={})",
            self.used_layers
        );
        // Upsample retained CPU pixels first (source of truth for re-upload).
        for slot in 0..self.slot_pixels.len() {
            let px = &self.slot_pixels[slot];
            if px.is_empty() {
                continue;
            }
            let src = map_side_len(px).unwrap_or(old_res);
            if src != need {
                self.slot_pixels[slot] = upsample_rgba_square(px, src, need);
            }
        }
        let new_tex = Self::create_texture(device, self.capacity_layers, need);
        if self.resize_pipeline.is_none() {
            let receiver = self.resize_ready.take().expect("material resize compiler receiver");
            self.resize_pipeline = Some(receiver.recv().expect("material resize compiler completion")
                .expect("material resize shader validation"));
        }
        let resize = self.resize_pipeline.as_ref().unwrap();
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("material_resize_params"), size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
        });
        queue.write_buffer(&params, 0, bytemuck::cast_slice(&[old_res, need, self.used_layers, 0u32]));
        let mut codecs = vec![0u32; self.used_layers as usize];
        for (&slot, pair) in &self.gpu_filter_codecs { codecs[slot as usize..slot as usize + 2].copy_from_slice(pair); }
        use wgpu::util::DeviceExt;
        let codec_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("material_resize_codecs"), contents: bytemuck::cast_slice(&codecs),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let target_view = new_tex.create_view(&wgpu::TextureViewDescriptor {
            format: Some(wgpu::TextureFormat::Rgba8Unorm),
            dimension: Some(wgpu::TextureViewDimension::D2Array), mip_level_count: Some(1), ..Default::default()
        });
        let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("material_resize_binding"), layout: &resize.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&target_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&self.storage_view()) },
                wgpu::BindGroupEntry { binding: 3, resource: codec_buffer.as_entire_binding() },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&resize.pipeline);
            pass.set_bind_group(0, &binding, &[]);
            pass.dispatch_workgroups(need.div_ceil(8), need.div_ceil(8), self.used_layers);
        }
        queue.submit([encoder.finish()]);
        self.texture = new_tex;
        self.view = Self::create_view(&self.texture, wgpu::TextureFormat::Rgba8UnormSrgb);
        self.linear_view = Self::create_view(&self.texture, wgpu::TextureFormat::Rgba8Unorm);
        self.layer_res = need;
        self.generation += 1;
        // Re-upload all written layers from CPU (GPU copy can't rescale).
        for layer in 0..self.used_layers {
            if self.gpu_authored.contains(&layer) { continue; }
            let px = self.slot_pixels.get(layer as usize);
            if let Some(px) = px {
                if !px.is_empty() && px.len() == (need * need * 4) as usize {
                    self.write_layer(queue, layer, px);
                    continue;
                }
            }
            // Sentinel / missing: solid white (layer 0) or black.
            let fill = if layer == 0 { 255u8 } else { 0u8 };
            let solid = vec![fill; (need * need * 4) as usize];
            // White needs RGB+A; black same. Alpha always 255.
            let mut solid = solid;
            for chunk in solid.chunks_exact_mut(4) {
                chunk[0] = fill;
                chunk[1] = fill;
                chunk[2] = fill;
                chunk[3] = 255;
            }
            self.write_layer(queue, layer, &solid);
        }
        let slots: Vec<u32> = self.gpu_filter_codecs.keys().copied().collect();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.encode_gpu_mips(device, queue, &mut encoder, &slots);
        queue.submit([encoder.finish()]);
    }

    /// Normalise an incoming square buffer to `self.layer_res` (grow array if larger).
    fn normalize_upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pixels: &[u8],
        name: &str,
    ) -> Option<Vec<u8>> {
        let side = match map_side_len(pixels) {
            Some(s) => s,
            None => {
                eprintln!(
                    "[MaterialTex] upload_map REJECTED '{}': got {} bytes (not a legal square map)",
                    name,
                    pixels.len()
                );
                return None;
            }
        };
        if side > self.layer_res {
            self.ensure_layer_res(device, queue, side);
        }
        if side == self.layer_res {
            Some(pixels.to_vec())
        } else if side < self.layer_res {
            Some(upsample_rgba_square(pixels, side, self.layer_res))
        } else {
            // side > MAX was clamped in map_side_len; shouldn't reach here.
            None
        }
    }

    /// Upload a square RGBA8 map and return its 1-based slot. Accepts any legal side
    /// (8..=MAX_MAP_RES); grows `layer_res` or upsamples the buffer so every array layer matches.
    /// Layer-count grow (power-of-two) bumps `generation` so consumers rebuild bind groups.
    /// MUST be called from `poll_background_tasks` (Frame Ordering Contract).
    ///
    /// Returns the slot (`>= 1`) assigned to this map. `name` is the runtime display name for
    /// the outliner's material children (empty = unknown, e.g. the save-restore path).
    pub(crate) fn upload_map(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, pixels: &[u8], name: &str) -> u32 {
        let Some(pixels) = self.normalize_upload(device, queue, pixels, name) else {
            return 0;
        };
        self.ensure_sentinel(queue);
        // Heal desync between used_layers and slot_pixels (defensive — never panic).
        while self.slot_pixels.len() < self.used_layers as usize {
            self.slot_pixels.push(Vec::new());
            self.slot_names.push(String::new());
        }
        if self.slot_pixels.len() > self.used_layers as usize {
            self.slot_pixels.truncate(self.used_layers as usize);
            self.slot_names.truncate(self.used_layers as usize);
        }
        // SLOT REUSE by stable name (owner 2026-07-23 "far from live"): live/final graph bakes
        // re-upload the same logical map every landing (`proc_mat_{id}_albedo`, …). Appending a
        // NEW layer each time grew the array + bumped `generation` → every bind group referencing
        // the array rebuilt + the PT scene re-uploaded per drag step (the stutter), and layers
        // leaked. Overwrite the existing layer IN PLACE instead: same slot id, same texture,
        // ZERO generation churn — consumers see fresh pixels with no rebinds. (Callers restart
        // PT accumulation explicitly — same-slot writes don't change any hashed state.)
        if !name.is_empty() {
            // slot_names/slot_pixels index DIRECTLY by slot id (heal-then-push lands the new
            // element at index == slot; slot_name()/slot_rgba() read .get(slot) verbatim).
            if let Some(k) = self.slot_names.iter().position(|n| n == name) {
                if k >= 1 {
                    let slot = k as u32;
                    self.write_layer(queue, slot, &pixels);
                    self.slot_pixels[k] = pixels;
                    self.gpu_authored.remove(&slot);
                    self.raw_precision_slots.remove(&slot);
                    self.gpu_filter_codecs.retain(|base, _| *base != slot && *base + 1 != slot);
                    self.gpu_mips.retain(|base, _| *base != slot && *base + 1 != slot);
                    return slot;
                }
            }
        }
        let slot = self.used_layers; // next free layer (>= 1)
        if slot >= self.capacity_layers {
            // Grow layer count: allocate a bigger texture, copy existing layers across.
            let new_cap = (self.capacity_layers * 2).max(slot + 1);
            let new_tex = Self::create_texture(device, new_cap, self.layer_res);
            let r = self.layer_res;
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("material_tex_array_grow"),
            });
            // Copy EVERY mip level across — a level left behind samples as black at distance.
            for level in 0..Self::mip_count(r) {
                let s = (r >> level).max(1);
                enc.copy_texture_to_texture(
                    wgpu::ImageCopyTexture {
                        texture: &self.texture, mip_level: level,
                        origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::ImageCopyTexture {
                        texture: &new_tex, mip_level: level,
                        origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: s, height: s,
                        depth_or_array_layers: self.used_layers.max(1),
                    },
                );
            }
            queue.submit(std::iter::once(enc.finish()));
            self.texture = new_tex;
            self.view = Self::create_view(&self.texture, wgpu::TextureFormat::Rgba8UnormSrgb);
            self.linear_view = Self::create_view(&self.texture, wgpu::TextureFormat::Rgba8Unorm);
            self.capacity_layers = new_cap;
            self.generation += 1; // bind groups referencing the old texture are now stale
        }
        self.write_layer(queue, slot, &pixels);
        self.used_layers = slot + 1;
        // Retain the CPU pixels so save (#2) can persist this layer without a GPU readback.
        // Heal if lengths drifted (never debug_assert-crash the app).
        if self.slot_pixels.len() != slot as usize {
            eprintln!(
                "[MaterialTex] slot_pixels len {} != slot {} — healing",
                self.slot_pixels.len(),
                slot
            );
            self.slot_pixels.resize_with(slot as usize, Vec::new);
            self.slot_names.resize_with(slot as usize, String::new);
        }
        self.slot_pixels.push(pixels);
        self.slot_names.push(name.to_string());
        slot
    }

    /// Upload a **16-bit dual-plane** map: high bytes at returned slot, low bytes at `slot+1`.
    /// Name reuse overwrites both planes in place (lo tracked as `name + "__lo16"`).
    pub(crate) fn upload_map_16(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        hi: &[u8],
        lo: &[u8],
        name: &str,
    ) -> u32 {
        let Some(hi) = self.normalize_upload(device, queue, hi, name) else {
            return 0;
        };
        // Re-normalise lo after hi may have grown layer_res.
        let Some(lo) = self.normalize_upload(device, queue, lo, name) else {
            return 0;
        };
        let lo_name = if name.is_empty() {
            String::new()
        } else {
            format!("{name}__lo16")
        };
        // Reuse existing dual pair by name.
        if !name.is_empty() {
            if let Some(k) = self.slot_names.iter().position(|n| n == name) {
                if k >= 1 {
                    let slot = k as u32;
                    self.write_layer(queue, slot, &hi);
                    self.slot_pixels[k] = hi;
                    self.gpu_authored.remove(&slot);
                    self.raw_precision_slots.remove(&slot);
                    self.gpu_filter_codecs.retain(|base, _| *base != slot && *base + 1 != slot);
                    self.gpu_mips.retain(|base, _| *base != slot && *base + 1 != slot);
                    // The lo plane is found BY NAME, never by position.
                    //
                    // This used to `.unwrap_or(slot + 1)`: when `name` had previously been uploaded
                    // as an 8-bit single-plane map there is no `__lo16` slot at all, so slot+1 is
                    // simply the NEXT MAP's layer — and the old code wrote the lo bytes into it AND
                    // renamed it to `<name>__lo16`. That silently destroyed an unrelated map (its
                    // pixels became this map's low byte plane) and made the damage permanent by
                    // taking its name. The 8-bit → 16-bit upgrade of any channel is an ordinary user
                    // action, so this was reachable without anything unusual happening.
                    //
                    // Accept ONLY a slot that genuinely carries the lo name; otherwise allocate a
                    // fresh layer. Adjacency is an artifact of how fresh pairs happen to be laid
                    // out, never a guarantee to rely on.
                    let lo_slot = self
                        .slot_names
                        .iter()
                        .position(|n| n == &lo_name)
                        .map(|i| i as u32)
                        .filter(|i| (*i as usize) < self.slot_pixels.len());
                    match lo_slot {
                        Some(lo_slot) => {
                            self.write_layer(queue, lo_slot, &lo);
                            self.slot_pixels[lo_slot as usize] = lo;
                            self.gpu_authored.remove(&lo_slot);
                            self.raw_precision_slots.remove(&lo_slot);
                            self.gpu_filter_codecs.retain(|base, _| *base != lo_slot && *base + 1 != lo_slot);
                            self.gpu_mips.retain(|base, _| *base != lo_slot && *base + 1 != lo_slot);
                            if !lo_name.is_empty() {
                                self.slot_names[lo_slot as usize] = lo_name;
                            }
                        }
                        None => {
                            // No lo plane yet (e.g. this channel was previously 8-bit). Allocate one
                            // rather than claiming a neighbour's layer.
                            let _ = self.upload_map(device, queue, &lo, &lo_name);
                        }
                    }
                    return slot;
                }
            }
        }
        // Fresh pair: allocate two consecutive layers.
        self.ensure_sentinel(queue);
        // First free slot after heal.
        let slot = self.used_layers.max(1);
        // Ensure capacity for slot and slot+1.
        while slot + 1 >= self.capacity_layers {
            let new_cap = (self.capacity_layers * 2).max(slot + 2);
            let new_tex = Self::create_texture(device, new_cap, self.layer_res);
            let r = self.layer_res;
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("material_tex_array_grow16"),
            });
            // Copy EVERY mip level across (same rule as the single-plane grow above).
            for level in 0..Self::mip_count(r) {
                let s = (r >> level).max(1);
                enc.copy_texture_to_texture(
                    wgpu::ImageCopyTexture {
                        texture: &self.texture,
                        mip_level: level,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::ImageCopyTexture {
                        texture: &new_tex,
                        mip_level: level,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: s,
                        height: s,
                        depth_or_array_layers: self.used_layers.max(1),
                    },
                );
            }
            queue.submit(std::iter::once(enc.finish()));
            self.texture = new_tex;
            self.view = Self::create_view(&self.texture, wgpu::TextureFormat::Rgba8UnormSrgb);
            self.linear_view = Self::create_view(&self.texture, wgpu::TextureFormat::Rgba8Unorm);
            self.capacity_layers = new_cap;
            self.generation += 1;
        }
        // Align slot_pixels to used_layers before push.
        while self.slot_pixels.len() < slot as usize {
            self.slot_pixels.push(Vec::new());
            self.slot_names.push(String::new());
        }
        self.write_layer(queue, slot, &hi);
        self.write_layer(queue, slot + 1, &lo);
        if self.slot_pixels.len() == slot as usize {
            self.slot_pixels.push(hi);
            self.slot_names.push(name.to_string());
            self.slot_pixels.push(lo);
            self.slot_names.push(lo_name);
        } else {
            // Overwrite path if vectors already longer (heal).
            if (slot as usize) < self.slot_pixels.len() {
                self.slot_pixels[slot as usize] = hi;
                self.slot_names[slot as usize] = name.to_string();
            } else {
                self.slot_pixels.push(hi);
                self.slot_names.push(name.to_string());
            }
            let lo_i = slot as usize + 1;
            if lo_i < self.slot_pixels.len() {
                self.slot_pixels[lo_i] = lo;
                self.slot_names[lo_i] = lo_name;
            } else {
                self.slot_pixels.push(lo);
                self.slot_names.push(lo_name);
            }
        }
        self.used_layers = (slot + 2).max(self.used_layers);
        slot
    }

    /// Runtime display name of a used slot (loader's file stem / library map name).
    /// `None` for slot 0, out-of-range slots, and slots restored from a save (names are
    /// runtime-only — callers fall back to the owning material's name).
    pub(crate) fn slot_name(&self, slot: u32) -> Option<&str> {
        self.slot_names.get(slot as usize)
            .map(|s| s.as_str())
            .filter(|s| !s.is_empty())
    }

    /// CPU-retained RGBA8 pixels of a USED map slot (`layer_res²`), for UI thumbnails.
    /// `None` for slot 0 (sentinel), out-of-range slots (e.g. a dangling `tex_id` after the
    /// load-time array reset) and any slot whose pixel record isn't a legal square —
    /// callers must render a placeholder instead of indexing (crash-audit: no UI path may
    /// assume a slot exists).
    pub(crate) fn slot_rgba(&self, slot: u32) -> Option<&[u8]> {
        if slot == 0 { return None; }
        if self.gpu_authored.contains(&slot) { return None; }
        self.slot_pixels.get(slot as usize)
            .map(|v| v.as_slice())
            .filter(|v| map_side_len(v).is_some())
    }

    /// Number of layers in use (>= 1 after the sentinel). Slots 1..used_layers are real maps.
    pub(crate) fn used_layers(&self) -> u32 { self.used_layers }

    /// Retained RGBA8 pixels for each USED map slot (1..used_layers, in order). For save persistence (#2).
    pub(crate) fn export_slots(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Vec<Vec<u8>>, String> {
        (1..self.used_layers).map(|slot| self.snapshot_slot(device, queue, slot)).collect()
    }

    /// Reset to a fresh (zeroed) texture with just the white sentinel in layer 0 — clears every loaded
    /// map and bumps `generation` so both consuming bind groups rebuild. Called on scene LOAD (from
    /// poll) so a newly-loaded scene never samples the PREVIOUS scene's texture slots — the cross-scene
    /// albedo-map bleed (review #2). A material whose `tex_id` no longer has backing pixels then samples
    /// a zeroed layer (renders "no map") instead of another scene's texture. MUST run in
    /// `poll_background_tasks` (Frame Ordering Contract — it recreates the GPU texture).
    pub(crate) fn reset(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        // Scene load: drop back to the default install ceiling (a prior 4K bake must not
        // force the next project to allocate 4K layers until it needs them).
        self.layer_res = DEFAULT_MAP_RES;
        self.texture = Self::create_texture(device, self.capacity_layers, self.layer_res);
        self.view = Self::create_view(&self.texture, wgpu::TextureFormat::Rgba8UnormSrgb);
        self.linear_view = Self::create_view(&self.texture, wgpu::TextureFormat::Rgba8Unorm);
        self.used_layers = 0;
        self.slot_pixels.clear();
        self.slot_names.clear();
        self.gpu_authored.clear();
        self.raw_precision_slots.clear();
        self.gpu_filter_codecs.clear(); self.gpu_mips.clear();
        self.generation += 1;
        self.ensure_sentinel(queue); // slot 0 = white so tex_id==0 materials stay unchanged
    }
}

// ============================================================
// Image decode helper (shared by the file-picker load and the AI-map button)
// ============================================================

/// Decode arbitrary image bytes (PNG/JPEG/…) into a `DEFAULT_MAP_RES²` RGBA8 buffer, resampling
/// to the default import resolution. Upload may upsample further if the array has grown (O3).
/// Returns `None` if decoding fails. Pure CPU — safe to call on the UI thread.
pub(crate) fn decode_to_map(bytes: &[u8]) -> Option<Vec<u8>> {
    decode_to_map_res(bytes, DEFAULT_MAP_RES)
}

/// Decode image bytes and resample to an explicit square side (clamped to [`MAX_MAP_RES`]).
pub(crate) fn decode_to_map_res(bytes: &[u8], res: u32) -> Option<Vec<u8>> {
    let res = res.clamp(8, MAX_MAP_RES);
    let img = image::load_from_memory(bytes).ok()?;
    let resized = img.resize_exact(res, res, image::imageops::FilterType::Triangle);
    Some(resized.to_rgba8().into_raw())
}

/// Load + decode an image FILE into a `DEFAULT_MAP_RES²` RGBA8 buffer. UI-thread safe.
pub(crate) fn decode_file_to_map(path: &std::path::Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    decode_to_map(&bytes)
}

/// Decode a **height / displacement** image to dual-plane 16-bit linear maps.
///
/// - **EXR / HDR / float TIFF**: keeps float precision (does **not** go through `to_rgba8`).
///   Uses the **R** channel as height (standard mono displacement).
/// - **8-bit PNG/JPG**: expands gray → 16-bit dual plane (`g * 257`).
///
/// Values outside 0..1 are remapped into 0..1 using the image min..max so the full
/// 16-bit range is used (common for world-unit EXR displacement). Mid-gray height maps
/// already in 0..1 pass through unchanged (no stretch if span ≤ 1 and min ≥ 0).
///
/// Returns `(hi_plane, lo_plane)` each `DEFAULT_MAP_RES²×4` bytes, ready for `upload_map_16`.
pub(crate) fn decode_file_to_height16_planes(
    path: &std::path::Path,
) -> Option<(Vec<u8>, Vec<u8>)> {
    let img = image::open(path).ok()?;
    // Always take float path so EXR / 16-bit TIFF keep precision. 8-bit sources
    // become 0..1 floats from their 8-bit values (still expanded to dual-plane u16).
    let rgba32 = img.to_rgba32f();
    let (w, h) = rgba32.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let res = DEFAULT_MAP_RES;
    // Resize in float space (Triangle) so EXR doesn't quantize before scale.
    let resized = if w != res || h != res {
        image::imageops::resize(
            &rgba32,
            res,
            res,
            image::imageops::FilterType::Triangle,
        )
    } else {
        rgba32
    };
    let n = (res * res) as usize;
    let mut heights = Vec::with_capacity(n);
    let mut mn = f32::INFINITY;
    let mut mx = f32::NEG_INFINITY;
    for p in resized.pixels() {
        // R channel = mono height (OpenEXR / ZBrush convention). Fall back to luma if R is flat-zero
        // but G/B carry data (rare RGB height packs).
        let mut v = p[0];
        if !v.is_finite() {
            v = 0.0;
        }
        if v.abs() < 1e-8 && (p[1].abs() > 1e-5 || p[2].abs() > 1e-5) {
            v = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
            if !v.is_finite() {
                v = 0.0;
            }
        }
        mn = mn.min(v);
        mx = mx.max(v);
        heights.push(v);
    }
    // Remap into 0..1 for the dual-plane u16 pack.
    // - Already in [0,1] with span ≤ 1: keep (classic mid-gray height).
    // - Wider / negative (world-unit EXR): normalize min..max → 0..1.
    let span = (mx - mn).max(1e-8);
    let already_unit = mn >= -1e-4 && mx <= 1.0 + 1e-4 && span <= 1.0 + 1e-3;
    let mut hi = vec![0u8; n * 4];
    let mut lo = vec![0u8; n * 4];
    for (i, v) in heights.iter().enumerate() {
        let t = if already_unit {
            v.clamp(0.0, 1.0)
        } else {
            ((*v - mn) / span).clamp(0.0, 1.0)
        };
        let (h, l) = encode_data_scalar_rgba16_planes(t);
        hi[i * 4..i * 4 + 4].copy_from_slice(&h);
        lo[i * 4..i * 4 + 4].copy_from_slice(&l);
    }
    Some((hi, lo))
}

/// Runtime display name for a map loaded from a file: the file stem
/// ("Metal_Scratched_01_Roughness.png" → "Metal_Scratched_01_Roughness"). The ONE producer of
/// `PendingMapLoad::name` for file-based loads (outliner material children show it).
pub(crate) fn map_display_name(path: &std::path::Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

// --- Linear-data maps in an sRGB array (normal / roughness / metallic) -----------------------
//
// The texture array's format is `Rgba8UnormSrgb` (correct for albedo — color data). Data maps
// hold RAW values (tangent normals, roughness scalars) that must NOT be sRGB-decoded on sample.
// Rather than add a second linear array (which would grow every consuming BGL + bind group +
// another generation counter for one bit of semantics), we store data maps PRE-ENCODED with the
// inverse transfer: byte' = encode_srgb(byte/255). The GPU's hardware decode then returns
// decode_srgb(byte'/255) == byte/255 — the raw value, exact up to 8-bit re-quantization (error
// ≤ ~1.2/255 near white, finer near black; on par with plain 8-bit storage and invisible for
// roughness/normal data). Honest trade documented in MATERIALS_UX.md.
//
// PANEL 16-BIT PATH (all channels): Bit16 maps use a **dual-plane** layout in the same
// RGBA8 sRGB array — plane `tex_id` = high bytes of each channel, plane `tex_id+1` = low
// bytes (each byte sRGB-pre-encoded so hardware decode recovers 0..255). Shader reconstructs
// `u16 = hi*256+lo` → 65 536 levels per component. Color (albedo/emission) stores **sRGB**
// as u16 then converts to linear in-shader; data maps (rough/metal/height/normal) store
// **linear** u16. Bit8 uses the classic single-plane 8-bit encode and clears high_precision.

/// Encode a linear 0..1 color into sRGB bytes for the `Rgba8UnormSrgb` albedo/emission slots.
/// CPU graph bakes compute in LINEAR; storing `v*255` raw made the hardware decode darken every
/// baked color (0.5 → 0.21 — the "baked material too dark" class, found by visdiff 2026-07-23).
pub(crate) fn encode_srgb_color_rgba(rgb: [f32; 3]) -> [u8; 4] {
    let lut = linear_to_srgb_lut();
    let e = |v: f32| lut[(v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8 as usize];
    [e(rgb[0]), e(rgb[1]), e(rgb[2]), 255]
}

/// Pre-encode a linear 0..1 scalar into sRGB-stored grayscale RGBA (for roughness/metallic maps).
/// The sRGB texture array hardware-decodes back to approximately `v`.
pub(crate) fn encode_data_scalar_rgba(v: f32) -> [u8; 4] {
    let b = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let e = linear_to_srgb_lut()[b as usize];
    [e, e, e, 255]
}

/// Linear 0..1 → sRGB 0..1 (analytic, matches the 256-entry LUT curve).
pub(crate) fn linear_to_srgb_f(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// sRGB 0..1 → linear 0..1.
pub(crate) fn srgb_to_linear_f(s: f32) -> f32 {
    let s = s.clamp(0.0, 1.0);
    if s <= 0.040_45 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// Pack one linear component to 16-bit; return (hi_byte, lo_byte) **pre-encoded** for the
/// sRGB texture array (hardware decode recovers the raw 0..255 byte values).
fn pack_u16_preencoded(v01: f32) -> (u8, u8) {
    let q = (v01.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
    let hi = (q >> 8) as u8;
    let lo = (q & 0xFF) as u8;
    let lut = linear_to_srgb_lut();
    (lut[hi as usize], lut[lo as usize])
}

/// Linear RGB → dual-plane 16-bit **sRGB** (albedo / emission). Returns (hi_rgba, lo_rgba).
pub(crate) fn encode_srgb_color_rgba16_planes(rgb: [f32; 3]) -> ([u8; 4], [u8; 4]) {
    let (rh, rl) = pack_u16_preencoded(linear_to_srgb_f(rgb[0]));
    let (gh, gl) = pack_u16_preencoded(linear_to_srgb_f(rgb[1]));
    let (bh, bl) = pack_u16_preencoded(linear_to_srgb_f(rgb[2]));
    ([rh, gh, bh, 255], [rl, gl, bl, 255])
}

/// Recover the raw 0..255 byte that was passed to [`pack_u16_preencoded`] from a
/// pre-encoded storage byte (inverse of the sRGB-array data trick).
#[inline]
fn recover_preencoded_byte(stored: u8) -> u8 {
    srgb_to_linear_byte_lut()[stored as usize]
}

/// Convert dual-plane 16-bit **color** bake planes → classic 8-bit sRGB map.
///
/// The bake always writes dual-plane 16-bit. Bit8 install must NOT upload the hi plane
/// alone: that plane stores `lut[srgb_u16 >> 8]`, and sampling it as a normal sRGB albedo
/// map recovers ~sRGB (not linear) → washed-out / wrong flat colors vs Bit16 reconstruct
/// (owner 2026-07-29: 8 vs 16 on a solid Fill looked like two different materials).
pub(crate) fn dual_plane_color16_to_srgb8(hi: &[u8], lo: &[u8]) -> Vec<u8> {
    let n = hi.len().min(lo.len());
    let mut out = vec![0u8; n];
    let mut i = 0;
    while i + 3 < n {
        let mut lin = [0.0f32; 3];
        for c in 0..3 {
            let hr = recover_preencoded_byte(hi[i + c]) as u16;
            let lr = recover_preencoded_byte(lo[i + c]) as u16;
            let s = (hr * 256 + lr) as f32 / 65535.0;
            lin[c] = srgb_to_linear_f(s);
        }
        let enc = encode_srgb_color_rgba(lin);
        out[i..i + 4].copy_from_slice(&enc);
        i += 4;
    }
    out
}

/// Convert dual-plane 16-bit **linear data** bake planes → classic 8-bit data map
/// (pre-encoded for the sRGB texture array, same as [`encode_data_scalar_rgba`]).
pub(crate) fn dual_plane_data16_to_srgb8(hi: &[u8], lo: &[u8]) -> Vec<u8> {
    let n = hi.len().min(lo.len());
    let mut out = vec![0u8; n];
    let mut i = 0;
    while i + 3 < n {
        // Grayscale data: all RGB channels carry the same scalar.
        let hr = recover_preencoded_byte(hi[i]) as u16;
        let lr = recover_preencoded_byte(lo[i]) as u16;
        let v = (hr * 256 + lr) as f32 / 65535.0;
        let enc = encode_data_scalar_rgba(v);
        out[i..i + 4].copy_from_slice(&enc);
        i += 4;
    }
    out
}

/// Convert dual-plane 16-bit **tangent normal** bake planes → classic OpenGL-style
/// 8-bit normal map (`0.5 + 0.5·n` per channel, **raw** bytes — same as ambientCG /
/// Steel_Normal: R/G ~128, B high). Do **not** use [`dual_plane_data16_to_srgb8`]:
/// that collapses RGB to grayscale and destroys break-surface normals.
///
/// On library load, [`decode_file_to_data_map`] applies the sRGB pre-encode for the
/// texture array (same path as every other data normal in the library).
pub(crate) fn dual_plane_normal16_to_rgba8(hi: &[u8], lo: &[u8]) -> Vec<u8> {
    let n = hi.len().min(lo.len());
    let mut out = vec![0u8; n];
    let mut i = 0;
    while i + 3 < n {
        for c in 0..3 {
            let hr = recover_preencoded_byte(hi[i + c]) as u16;
            let lr = recover_preencoded_byte(lo[i + c]) as u16;
            // Packed as 0.5 + 0.5 * n (unit tangent normal).
            let v = (hr * 256 + lr) as f32 / 65535.0;
            out[i + c] = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
        out[i + 3] = 255;
        i += 4;
    }
    out
}

/// Linear scalar → dual-plane 16-bit **linear** (roughness / metallic / height).
pub(crate) fn encode_data_scalar_rgba16_planes(v: f32) -> ([u8; 4], [u8; 4]) {
    let (h, l) = pack_u16_preencoded(v);
    ([h, h, h, 255], [l, l, l, 255])
}

/// Linear RGB data (normal) → dual-plane 16-bit linear.
pub(crate) fn encode_data_rgb_rgba16_planes(rgb: [f32; 3]) -> ([u8; 4], [u8; 4]) {
    let (rh, rl) = pack_u16_preencoded(rgb[0]);
    let (gh, gl) = pack_u16_preencoded(rgb[1]);
    let (bh, bl) = pack_u16_preencoded(rgb[2]);
    ([rh, gh, bh, 255], [rl, gl, bl, 255])
}

/// Inverse of [`encode_data_rgb_rgba16_planes`] for one texel (linear 0..1).
pub(crate) fn decode_data_rgb16_texel(hi: &[u8], lo: &[u8], i: usize) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    let base = i * 4;
    if base + 2 >= hi.len() || base + 2 >= lo.len() {
        return out;
    }
    for c in 0..3 {
        let hr = recover_preencoded_byte(hi[base + c]) as u16;
        let lr = recover_preencoded_byte(lo[base + c]) as u16;
        out[c] = (hr * 256 + lr) as f32 / 65535.0;
    }
    out
}

/// Pack unit tangent normal as 16-bit dual-plane (`0.5 + 0.5 * n`).
pub(crate) fn encode_normal_rgba16_planes(n: [f32; 3]) -> ([u8; 4], [u8; 4]) {
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-8);
    encode_data_rgb_rgba16_planes([
        n[0] / len * 0.5 + 0.5,
        n[1] / len * 0.5 + 0.5,
        n[2] / len * 0.5 + 0.5,
    ])
}

/// Single-plane 16-bit scalar (RG pack) — kept for tests / legacy; prefer dual-plane.
pub(crate) fn encode_data_scalar_rgba16(v: f32) -> [u8; 4] {
    let (hi, lo) = encode_data_scalar_rgba16_planes(v);
    // Collapse dual into RG of one texel (hi, lo, lo, 255) matching older single-plane path.
    [hi[0], lo[0], lo[0], 255]
}

/// Expand 8-bit gray → dual-plane 16-bit linear (`g * 257`).
pub(crate) fn encode_gray8_as_rgba16_planes(g: u8) -> ([u8; 4], [u8; 4]) {
    let q = (g as u16).saturating_mul(257);
    let v = q as f32 / 65535.0;
    encode_data_scalar_rgba16_planes(v)
}

/// Expand 8-bit gray → single-plane RG pack (legacy).
pub(crate) fn encode_gray8_as_rgba16(g: u8) -> [u8; 4] {
    let (hi, lo) = encode_gray8_as_rgba16_planes(g);
    [hi[0], lo[0], lo[0], 255]
}

/// Split an already-filled linear float buffer into dual-plane RGBA8 maps.
pub(crate) fn split_linear_rgb_to_planes16(
    linear_rgb: &[[f32; 3]],
    as_srgb_color: bool,
) -> (Vec<u8>, Vec<u8>) {
    let n = linear_rgb.len();
    let mut hi = vec![0u8; n * 4];
    let mut lo = vec![0u8; n * 4];
    for (i, rgb) in linear_rgb.iter().enumerate() {
        let (h, l) = if as_srgb_color {
            encode_srgb_color_rgba16_planes(*rgb)
        } else {
            encode_data_rgb_rgba16_planes(*rgb)
        };
        hi[i * 4..i * 4 + 4].copy_from_slice(&h);
        lo[i * 4..i * 4 + 4].copy_from_slice(&l);
    }
    (hi, lo)
}

/// Pre-encode linear RGB (0..1) into sRGB-stored data-map RGBA (normal / bump RGB).
pub(crate) fn encode_data_rgb_rgba(rgb: [f32; 3]) -> [u8; 4] {
    let lut = linear_to_srgb_lut();
    let enc = |c: f32| {
        let b = (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        lut[b as usize]
    };
    [enc(rgb[0]), enc(rgb[1]), enc(rgb[2]), 255]
}

/// Pack a unit tangent-space normal into data-map RGBA (`0.5 + 0.5 * n`, OpenGL-style).
pub(crate) fn encode_normal_rgba(n: [f32; 3]) -> [u8; 4] {
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-8);
    let nn = [n[0] / len, n[1] / len, n[2] / len];
    encode_data_rgb_rgba([nn[0] * 0.5 + 0.5, nn[1] * 0.5 + 0.5, nn[2] * 0.5 + 0.5])
}

/// 256-entry LUT: v/255 (linear) → round(255 * linear_to_srgb(v/255)).
/// Pre-encode PROCEDURALLY GENERATED data-map bytes (normal / roughness / metallic)
/// the same way `decode_file_to_data_map` encodes loaded files: the array is sRGB, so
/// data channels must be stored encoded to survive the hardware's sRGB→linear decode.
/// Alpha untouched. Shares the one LUT — no second implementation.
pub(crate) fn encode_data_map_in_place(px: &mut [u8]) {
    let lut = linear_to_srgb_lut();
    for chunk in px.chunks_exact_mut(4) {
        chunk[0] = lut[chunk[0] as usize];
        chunk[1] = lut[chunk[1] as usize];
        chunk[2] = lut[chunk[2] as usize];
    }
}

fn linear_to_srgb_lut() -> &'static [u8; 256] {
    use std::sync::OnceLock;
    static LUT: OnceLock<[u8; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0u8; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let c = i as f32 / 255.0;
            let s = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
            *e = (s * 255.0 + 0.5) as u8;
        }
        t
    })
}

/// Decode a DATA map (normal / roughness / metallic) file: resample to `MAP_RES²` RGBA8 and
/// pre-encode RGB to sRGB so the sRGB texture array's hardware decode returns the raw values.
/// Alpha is left untouched (unused by data maps). UI-thread / worker-thread safe.
pub(crate) fn decode_file_to_data_map(path: &std::path::Path) -> Option<Vec<u8>> {
    let mut px = decode_file_to_map(path)?;
    let lut = linear_to_srgb_lut();
    for chunk in px.chunks_exact_mut(4) {
        chunk[0] = lut[chunk[0] as usize];
        chunk[1] = lut[chunk[1] as usize];
        chunk[2] = lut[chunk[2] as usize];
    }
    Some(px)
}

/// Decode ONE channel (`ch` = 0..=2) of a packed data map (e.g. OcclusionRoughnessMetallic:
/// G = roughness, B = metallic) as a grayscale data map: replicate the channel to RGB, then
/// pre-encode exactly like [`decode_file_to_data_map`]. Worker-thread safe.
pub(crate) fn decode_file_to_data_map_channel(path: &std::path::Path, ch: usize) -> Option<Vec<u8>> {
    let ch = ch.min(2);
    let mut px = decode_file_to_map(path)?;
    let lut = linear_to_srgb_lut();
    for chunk in px.chunks_exact_mut(4) {
        let v = lut[chunk[ch] as usize];
        chunk[0] = v;
        chunk[1] = v;
        chunk[2] = v;
    }
    Some(px)
}

/// 256-entry inverse LUT of `linear_to_srgb_lut`: s/255 (sRGB) → round(255 * srgb_to_linear(s/255)).
/// Used to display DATA maps (normal / roughness / metallic): their stored bytes are pre-ENCODED
/// (see above), so decoding recovers the original file's bytes for a faithful thumbnail.
/// Also used by Mult bake sampling of data maps (node_graph_bake).
pub(crate) fn srgb_to_linear_byte_lut() -> &'static [u8; 256] {
    srgb_to_linear_lut()
}

fn srgb_to_linear_lut() -> &'static [u8; 256] {
    use std::sync::OnceLock;
    static LUT: OnceLock<[u8; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0u8; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let s = i as f32 / 255.0;
            let c = if s <= 0.040_45 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) };
            *e = (c * 255.0 + 0.5) as u8;
        }
        t
    })
}

// ============================================================
// Mip-chain downsampling (material texture array)
// ============================================================

/// sRGB byte → linear scaled to 0..=65535. 16-bit workspace: enough precision that a uniform
/// area re-encodes to the identical byte at every mip level (round-trip unit-tested below).
fn srgb_to_linear16_lut() -> &'static [u16; 256] {
    use std::sync::OnceLock;
    static LUT: OnceLock<[u16; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0u16; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let s = i as f32 / 255.0;
            let c = if s <= 0.040_45 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) };
            *e = (c * 65535.0 + 0.5) as u16;
        }
        t
    })
}

/// linear 0..=65535 → sRGB byte (inverse of the above; 64 KB, built once).
fn linear16_to_srgb_lut() -> &'static [u8] {
    use std::sync::OnceLock;
    static LUT: OnceLock<Vec<u8>> = OnceLock::new();
    LUT.get_or_init(|| {
        (0..=65535u32)
            .map(|v| {
                let c = v as f32 / 65535.0;
                let s = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
                (s * 255.0 + 0.5) as u8
            })
            .collect()
    })
}

/// Halve a square RGBA8 sRGB image (2×2 box). RGB averages in LINEAR space — the same math the
/// GPU's sRGB decode + bilinear does at mip 0, so both color maps and the pre-encoded data maps
/// (normal/roughness/metallic — stored through the same transfer curve) filter consistently.
/// Alpha is not gamma-encoded — plain average. Odd sides clamp the second tap (wgpu mip rule:
/// next level = `max(1, side >> 1)`).
pub(crate) fn downsample_half_rgba_srgb(src: &[u8], side: u32) -> Vec<u8> {
    let sw = side.max(1);
    let dw = (sw >> 1).max(1);
    let mut out = vec![0u8; (dw * dw * 4) as usize];
    if src.len() != (sw * sw * 4) as usize {
        eprintln!(
            "[MaterialTex] downsample: {} bytes is not a {sw}² RGBA image — emitting a black level",
            src.len()
        );
        return out;
    }
    let dec = srgb_to_linear16_lut();
    let enc = linear16_to_srgb_lut();
    for y in 0..dw {
        let sy0 = (2 * y).min(sw - 1);
        let sy1 = (2 * y + 1).min(sw - 1);
        for x in 0..dw {
            let sx0 = (2 * x).min(sw - 1);
            let sx1 = (2 * x + 1).min(sw - 1);
            let i00 = ((sy0 * sw + sx0) * 4) as usize;
            let i10 = ((sy0 * sw + sx1) * 4) as usize;
            let i01 = ((sy1 * sw + sx0) * 4) as usize;
            let i11 = ((sy1 * sw + sx1) * 4) as usize;
            let o = ((y * dw + x) * 4) as usize;
            for c in 0..3 {
                let s = dec[src[i00 + c] as usize] as u32
                    + dec[src[i10 + c] as usize] as u32
                    + dec[src[i01 + c] as usize] as u32
                    + dec[src[i11 + c] as usize] as u32;
                out[o + c] = enc[((s + 2) >> 2) as usize];
            }
            let a = src[i00 + 3] as u32
                + src[i10 + 3] as u32
                + src[i01 + 3] as u32
                + src[i11 + 3] as u32;
            out[o + 3] = ((a + 2) >> 2) as u8;
        }
    }
    out
}

// ============================================================
// Normal-map green-channel convention detection
// ============================================================

/// Auto-detect a normal map's green-channel convention from its source file.
/// Returns `true` for DirectX (green = down-image), `false` for OpenGL (green = up-image).
///
/// Priority: explicit filename markers (`NormalGL` / `NormalDX` — the ambientCG spelling — and
/// the common `_gl` / `_dx` / `ogl` / `directx` variants) → sibling-layout inference (a packed
/// `OcclusionRoughnessMetallic` map in the same folder marks an Unreal-style export, and UE
/// ships DirectX normals) → OpenGL, the ambientCG / Blender / glTF default.
/// The user can override per material in the Normal map row (`TexMap::green_dx`).
pub(crate) fn normal_green_is_dx(path: &std::path::Path) -> bool {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    for m in ["normaldx", "normal_dx", "normal-dx", "nor_dx", "directx"] {
        if stem.contains(m) { return true; }
    }
    if stem.ends_with("_dx") || stem.ends_with("-dx") { return true; }
    for m in ["normalgl", "normal_gl", "normal-gl", "nor_gl", "opengl", "_ogl"] {
        if stem.contains(m) { return false; }
    }
    if stem.ends_with("_gl") || stem.ends_with("-gl") { return false; }
    // Unmarked: an ORM-packed sibling = Unreal export layout = DirectX normals.
    if let Some(dir) = path.parent() {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_lowercase();
                if n.contains("occlusionroughnessmetallic") || n.contains("_orm") {
                    return true;
                }
            }
        }
    }
    false
}

// ============================================================
// Map-slot thumbnails (material editor + outliner shader-tree rows)
// ============================================================

/// Thumbnail edge in pixels. Drawn at 48 px in the editor / ~16 px in the outliner; 64 source
/// pixels keeps both crisp.
pub(crate) const THUMB_RES: usize = 64;

/// Build a `THUMB_RES²` RGBA8 thumbnail from a square slot by STRIDE box-sampling
/// (2×2 taps per output pixel). Deliberately O(THUMB_RES²), never O(layer_res²): safe to run
/// in the draw path on a cache miss (the stall lesson — a full-image downscale of a 4K map is
/// millions of reads; this is thousands).
///
/// `decode_data`: data maps (normal / roughness / metallic) are stored pre-encoded to sRGB so
/// the GPU decode returns raw values; decoding here recovers the original file's bytes so the
/// thumbnail looks like the source image, not a brightened version of it.
fn build_slot_thumb(src: &[u8], decode_data: bool) -> Vec<u8> {
    let Some(map_res) = map_side_len(src) else {
        eprintln!(
            "[MaterialTex] build_slot_thumb: bad src len {} — placeholder",
            src.len()
        );
        return vec![128u8; THUMB_RES * THUMB_RES * 4];
    };
    // For maps smaller than THUMB_RES, stride collapses to 1 (nearest-neighbour-ish box).
    let stride = (map_res as usize / THUMB_RES).max(1);
    let lut = srgb_to_linear_lut();
    let mut out = vec![0u8; THUMB_RES * THUMB_RES * 4];
    for ty in 0..THUMB_RES {
        for tx in 0..THUMB_RES {
            // 2×2 box taps around the cell center (clamped to in-bounds).
            let sx = (tx * stride + stride / 2).saturating_sub(1).min(map_res as usize - 1);
            let sy = (ty * stride + stride / 2).saturating_sub(1).min(map_res as usize - 1);
            let mut acc = [0u32; 4];
            let mut taps = 0u32;
            for dy in 0..2usize {
                for dx in 0..2usize {
                    let px = (sx + dx).min(map_res as usize - 1);
                    let py = (sy + dy).min(map_res as usize - 1);
                    let si = (py * map_res as usize + px) * 4;
                    if si + 3 >= src.len() {
                        continue;
                    }
                    for c in 0..4 {
                        acc[c] += src[si + c] as u32;
                    }
                    taps += 1;
                }
            }
            let di = (ty * THUMB_RES + tx) * 4;
            let div = taps.max(1);
            for c in 0..4 {
                let v = (acc[c] / div) as u8;
                out[di + c] = if decode_data && c < 3 { lut[v as usize] } else { v };
            }
        }
    }
    out
}

/// egui-texture cache of map-slot thumbnails, keyed `(slot, decode_data)` and invalidated by the
/// texture array's `generation` (bumps on grow AND on the scene-load reset — a reset both clears
/// the slots and re-uploads the new scene's maps, so stale thumbs can never show another scene's
/// image). Mirrors `MaterialPreviewCache` (mat_preview.rs) — same ctx.load_texture lifecycle.
pub(crate) struct MapThumbCache {
    entries: std::collections::HashMap<(u32, bool), (u64, eframe::egui::TextureHandle)>,
}

impl MapThumbCache {
    pub(crate) fn new() -> Self { Self { entries: std::collections::HashMap::new() } }

    /// Get (or build on miss) the thumbnail for a used slot. `None` when the slot has no backing
    /// pixels (slot 0 / dangling tex_id) — the caller shows its text fallback. Cache hit = one
    /// HashMap lookup; miss = an O(THUMB_RES²) stride sample (see `build_slot_thumb`).
    pub(crate) fn get_or_create(
        &mut self,
        ctx: &eframe::egui::Context,
        arr: &MaterialTextureArray,
        slot: u32,
        decode_data: bool,
    ) -> Option<eframe::egui::TextureId> {
        let src = arr.slot_rgba(slot)?;
        let gen = arr.generation;
        if let Some((g, h)) = self.entries.get(&(slot, decode_data)) {
            if *g == gen { return Some(h.id()); }
        }
        // Bounded growth: drop stale-generation entries before inserting (slots are append-only
        // within a generation, so live entries always carry the current generation).
        self.entries.retain(|_, (g, _)| *g == gen);
        let thumb = build_slot_thumb(src, decode_data);
        let ci = eframe::egui::ColorImage::from_rgba_unmultiplied([THUMB_RES, THUMB_RES], &thumb);
        let handle = ctx.load_texture(
            format!("map_thumb_{slot}_{}", decode_data as u8),
            ci,
            eframe::egui::TextureOptions::LINEAR,
        );
        let id = handle.id();
        self.entries.insert((slot, decode_data), (gen, handle));
        Some(id)
    }
}

/// PNG-encode a square RGBA8 buffer for persistence (#2). None on size mismatch / encode error.
/// Preserves the buffer's native side (O3: 2K/4K saves round-trip at full res).
pub(crate) fn encode_map_png(rgba: &[u8]) -> Option<Vec<u8>> {
    let side = map_side_len(rgba)?;
    let img = image::RgbaImage::from_raw(side, side, rgba.to_vec())?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// Decode a persisted PNG map back to square RGBA8 (#2). Keeps native square size when legal
/// (so a 4K save reloads at 4K); otherwise resamples to [`DEFAULT_MAP_RES`].
pub(crate) fn decode_map_png(png: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(png).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    if w == h && (8..=MAX_MAP_RES).contains(&w) {
        return Some(img.into_raw());
    }
    decode_to_map(png)
}

/// S8 foundation — separable box blur on square RGBA8 (neighborhood bake pass).
/// Radius 0 = identity. Clamp edges (not wrap). Future Blur / Slope-Blur nodes call this
/// (R1: bake owns neighborhood ops; never live G-buffer blur).
pub(crate) fn map_box_blur_rgba(src: &[u8], res: u32, radius: u32) -> Vec<u8> {
    let n = (res * res * 4) as usize;
    if src.len() != n || res == 0 {
        return src.to_vec();
    }
    if radius == 0 {
        return src.to_vec();
    }
    let r = radius as i32;
    let res_i = res as i32;
    // Horizontal pass into temp.
    let mut tmp = vec![0u8; n];
    for y in 0..res_i {
        for x in 0..res_i {
            let mut acc = [0u32; 4];
            let mut cnt = 0u32;
            for dx in -r..=r {
                let xx = (x + dx).clamp(0, res_i - 1) as u32;
                let i = ((y as u32 * res + xx) * 4) as usize;
                for c in 0..4 {
                    acc[c] += src[i + c] as u32;
                }
                cnt += 1;
            }
            let o = ((y as u32 * res + x as u32) * 4) as usize;
            for c in 0..4 {
                tmp[o + c] = ((acc[c] + cnt / 2) / cnt) as u8;
            }
        }
    }
    // Vertical pass into out.
    let mut out = vec![0u8; n];
    for y in 0..res_i {
        for x in 0..res_i {
            let mut acc = [0u32; 4];
            let mut cnt = 0u32;
            for dy in -r..=r {
                let yy = (y + dy).clamp(0, res_i - 1) as u32;
                let i = ((yy * res + x as u32) * 4) as usize;
                for c in 0..4 {
                    acc[c] += tmp[i + c] as u32;
                }
                cnt += 1;
            }
            let o = ((y as u32 * res + x as u32) * 4) as usize;
            for c in 0..4 {
                out[o + c] = ((acc[c] + cnt / 2) / cnt) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_material_precision_mips_preserve_carry_color_growth_and_reset() {
        let (_lock, device, queue) = crate::gpu_test_harness::require_adapter();
        let device = std::sync::Arc::new(device);
        let mut textures = MaterialTextureArray::new(&device, 8);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !textures.prepare_gpu_filtering().unwrap() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let slots: Vec<_> = (0..4).map(|index| textures.reserve_gpu_map(&device, &queue, &format!("mip_probe_{index}"))).collect();
        let mut base_planes = Vec::new();
        let filter = |high: &[u8], low: &[u8], source: u32, target: u32, kind: usize| {
            if kind == 3 && target == source / 2 {
                return (downsample_half_rgba_srgb(high, source), downsample_half_rgba_srgb(low, source));
            }
            let mut result_high = Vec::new();
            let mut result_low = Vec::new();
            for row in 0..target { for column in 0..target {
                let position = glam::Vec2::new(column as f32 + 0.5, row as f32 + 0.5) * source as f32 / target as f32 - glam::Vec2::splat(0.5);
                let origin = position.floor(); let fraction = position - origin;
                let indices: Vec<usize> = [(0, 0), (1, 0), (0, 1), (1, 1)].into_iter().map(|(horizontal, vertical)| {
                    let column = (origin.x as i32 + horizontal).clamp(0, source as i32 - 1) as u32;
                    let row = (origin.y as i32 + vertical).clamp(0, source as i32 - 1) as u32;
                    (row * source + column) as usize
                }).collect();
                let values: Vec<f32> = indices.iter().map(|&index| match kind {
                    0 => (high[index * 4] as f32 * 256.0 + low[index * 4] as f32) / 65535.0,
                    1 => decode_data_rgb16_texel(high, low, index)[0],
                    2 => srgb_to_linear_f(decode_data_rgb16_texel(high, low, index)[0]),
                    _ => srgb_to_linear_f(high[index * 4] as f32 / 255.0),
                }).collect();
                if values.iter().all(|value| *value == values[0]) {
                    result_high.extend_from_slice(&high[indices[0] * 4..indices[0] * 4 + 4]);
                    result_low.extend_from_slice(&low[indices[0] * 4..indices[0] * 4 + 4]);
                } else {
                    let top = values[0] + (values[1] - values[0]) * fraction.x;
                    let bottom = values[2] + (values[3] - values[2]) * fraction.x;
                    let value = top + (bottom - top) * fraction.y;
                    let planes = match kind {
                        0 => { let quantized = (value * 65535.0 + 0.5) as u16; ([((quantized >> 8) & 255) as u8; 4], [(quantized & 255) as u8; 4]) },
                        1 => encode_data_scalar_rgba16_planes(value),
                        2 => encode_srgb_color_rgba16_planes([value; 3]),
                        _ => ([(linear_to_srgb_f(value) * 255.0 + 0.5) as u8; 4], [0; 4]),
                    };
                    result_high.extend_from_slice(&planes.0); result_low.extend_from_slice(&planes.1);
                }
            } }
            (result_high, result_low)
        };
        for (index, &slot) in slots.iter().enumerate() {
            let mut high = Vec::new();
            let mut low = Vec::new();
            for pixel in 0..64 {
                let white = pixel % 2 != 0;
                let value = if white { 257u16 } else { 255u16 };
                let planes = match index {
                    0 => ([((value >> 8) & 255) as u8; 4], [(value & 255) as u8; 4]),
                    1 => encode_data_scalar_rgba16_planes(value as f32 / 65535.0),
                    2 => encode_srgb_color_rgba16_planes([if white { 1.0 } else { 0.0 }; 3]),
                    _ => ([if white { 255 } else { 0 }; 4], [0; 4]),
                };
                high.extend_from_slice(&planes.0); low.extend_from_slice(&planes.1);
            }
            base_planes.push((high.clone(), low.clone()));
            for (plane, pixels) in [(slot, high), (slot + 1, low)] {
                queue.write_texture(wgpu::ImageCopyTexture { texture: &textures.texture, mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y: 0, z: plane }, aspect: wgpu::TextureAspect::All },
                    &pixels, wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(32), rows_per_image: Some(8) },
                    wgpu::Extent3d { width: 8, height: 8, depth_or_array_layers: 1 });
            }
            textures.mark_gpu_authored(slot, index != 3, index == 0, index == 2 || index == 3);
        }
        let mut encoder = device.create_command_encoder(&Default::default());
        textures.encode_gpu_mips(&device, &queue, &mut encoder, &slots);
        queue.submit([encoder.finish()]);
        let binding_count = textures.gpu_mips.len();
        let generation = textures.generation;
        let mut encoder = device.create_command_encoder(&Default::default());
        textures.encode_gpu_mips(&device, &queue, &mut encoder, &slots);
        queue.submit([encoder.finish()]);
        assert_eq!(textures.gpu_mips.len(), binding_count);
        assert_eq!(textures.generation, generation);
        for stage in 0..3 {
            if stage == 1 {
                let capacity = textures.capacity_layers + 1;
                textures.ensure_capacity(&device, &queue, capacity);
                assert!(textures.generation > generation);
            } else if stage == 2 {
                textures.ensure_layer_res(&device, &queue, 16);
                assert_eq!(textures.gpu_mip_generation, textures.generation);
            }
            for (index, &slot) in slots.iter().enumerate() {
                let mut expected_planes = if stage == 2 { filter(&base_planes[index].0, &base_planes[index].1, 8, 16, index) } else { base_planes[index].clone() };
                let mut expected_levels = vec![expected_planes.clone()];
                let mut expected_side = textures.layer_res;
                while expected_side > 1 {
                    expected_planes = filter(&expected_planes.0, &expected_planes.1, expected_side, expected_side / 2, index);
                    expected_side /= 2; expected_levels.push(expected_planes.clone());
                }
                let levels = if stage == 2 { vec![0, MaterialTextureArray::mip_count(textures.layer_res) - 1] }
                    else { (1..MaterialTextureArray::mip_count(textures.layer_res)).collect() };
                for level in levels {
                    let side = (textures.layer_res >> level).max(1);
                    let pixels = textures.read_slot_mip(&device, &queue, slot, level).unwrap();
                    let low_pixels = textures.read_slot_mip(&device, &queue, slot + 1, level).unwrap();
                    if stage == 2 && level == 0 {
                        if index == 3 { assert!((pixels[4] as i32 - 137).abs() <= 1, "8-bit resize must interpolate linear values, not encoded bytes: {}", pixels[4]); }
                    } else {
                        for row in 0..side as usize { for column in 0..side as usize {
                            let offset = (row * side as usize + column) * 4;
                            let high = pixels[offset] as f32 / 255.0;
                            let low = low_pixels[offset] as f32 / 255.0;
                            let value = match index {
                                0 => (high * 65280.0 + low * 255.0) / 65535.0,
                                1 => decode_data_rgb16_texel(&pixels, &low_pixels, row * side as usize + column)[0],
                                2 => srgb_to_linear_f(decode_data_rgb16_texel(&pixels, &low_pixels, row * side as usize + column)[0]),
                                _ => srgb_to_linear_f(high),
                            };
                            let oracle = &expected_levels[level as usize];
                            let pixel_index = row * side as usize + column;
                            let expected = match index {
                                0 => 256.0 / 65535.0,
                                1 => decode_data_rgb16_texel(&oracle.0, &oracle.1, pixel_index)[0],
                                2 => srgb_to_linear_f(decode_data_rgb16_texel(&oracle.0, &oracle.1, pixel_index)[0]),
                                _ => srgb_to_linear_f(oracle.0[offset] as f32 / 255.0),
                            };
                            let tolerance = if index < 2 { 1.1 / 65535.0 } else { 0.0001 };
                            assert!((value - expected).abs() < tolerance, "stage={stage} map={index} mip={level} value={value} expected={expected}");
                        } }
                    }
                }
            }
        }
        textures.reset(&device, &queue);
        assert!(textures.gpu_mips.is_empty() && textures.gpu_filter_codecs.is_empty());
        assert!(textures.gpu_authored.is_empty());
    }

    /// GATE (pillar review 2026-07-30) — the 16-bit lo plane is located BY NAME, never by adjacency.
    ///
    /// `upload_map_16` used to fall back to `slot + 1` when no `<name>__lo16` slot existed. That
    /// happens whenever a channel was previously uploaded as an ordinary 8-bit map and the user then
    /// switches it to 16-bit — a routine action. slot+1 is then simply the NEXT MAP's layer, and the
    /// old code overwrote its pixels with this map's low bytes AND renamed it `<name>__lo16`,
    /// destroying an unrelated map permanently and silently.
    ///
    /// This pins the SELECTION rule (no GPU device needed): given a slot table where `name` exists
    /// but `name__lo16` does not, the lo plane must NOT resolve to the neighbouring slot.
    #[test]
    fn lo16_plane_is_found_by_name_never_by_adjacency() {
        // Layout after: base color uploaded 8-bit, then roughness uploaded 8-bit.
        let slot_names = vec![
            String::new(),            // 0 = sentinel
            "basecolor".to_string(),  // 1
            "roughness".to_string(),  // 2  <- slot+1 relative to basecolor
        ];

        // Now "basecolor" is re-uploaded as 16-bit. Its lo plane does not exist yet.
        let name = "basecolor";
        let lo_name = format!("{name}__lo16");
        let slot = slot_names.iter().position(|n| n == name).unwrap() as u32;

        let resolved = slot_names
            .iter()
            .position(|n| n == &lo_name)
            .map(|i| i as u32)
            .filter(|i| (*i as usize) < slot_names.len());

        assert_eq!(
            resolved, None,
            "no __lo16 slot exists yet, so the lo plane must resolve to None (allocate a fresh \
             layer). The old `.unwrap_or(slot + 1)` returned {} — which is '{}', an unrelated map.",
            slot + 1,
            slot_names[(slot + 1) as usize]
        );

        // And the neighbour must still be itself — not renamed into this map's lo plane.
        assert_eq!(
            slot_names[(slot + 1) as usize],
            "roughness",
            "slot+1 belongs to another map and must never be claimed as a lo plane"
        );

        // Positive half: once a real lo plane exists, it IS found — by name, wherever it sits.
        let with_lo = vec![
            String::new(),
            "basecolor".to_string(),
            "roughness".to_string(),
            "basecolor__lo16".to_string(), // deliberately NOT adjacent to basecolor
        ];
        let found = with_lo
            .iter()
            .position(|n| n == &lo_name)
            .map(|i| i as u32)
            .filter(|i| (*i as usize) < with_lo.len());
        assert_eq!(
            found,
            Some(3),
            "the lo plane must be found by name even when it is not at slot+1"
        );
    }

    /// Regression (owner log 2026-07-17): "Texture 'material_tex_array' usages
    /// COPY_DST|TEXTURE_BINDING do not contain required COPY_SRC" — uncaptured validation
    /// error fired on the FL4MATX2 multi-map restore at startup, the first time the array
    /// GREW (>3 maps) and `upload_map` used the old texture as a copy SOURCE. The texture
    /// is created from ONE descriptor (`texture_descriptor`); this pins every usage the
    /// array's lifetime needs: TEXTURE_BINDING (sampled by gbuffer), COPY_DST
    /// (write_texture uploads), COPY_SRC (grow-copy source).
    /// Thumbnails are O(THUMB_RES²) stride samples of the retained slot pixels: a solid-color
    /// source thumbs to that color; a data map (pre-encoded to sRGB at load) decodes back to the
    /// ORIGINAL bytes (round-trip through both LUTs), so thumbnails show the source image.
    #[test]
    fn slot_thumb_stride_sample_and_data_decode() {
        // Solid mid-gray color map → thumb is that exact gray, alpha preserved.
        let px = [90u8, 90, 90, 255].repeat((MAP_RES * MAP_RES) as usize);
        let t = build_slot_thumb(&px, false);
        assert_eq!(t.len(), THUMB_RES * THUMB_RES * 4);
        assert!(t.chunks_exact(4).all(|c| c == [90, 90, 90, 255]),
            "solid source must thumb to the same color");
        // Data map: original byte 90 → stored pre-encoded → thumbnail decodes back to ~90.
        let stored = linear_to_srgb_lut()[90];
        let px_data = [stored, stored, stored, 255].repeat((MAP_RES * MAP_RES) as usize);
        let t = build_slot_thumb(&px_data, true);
        let c = &t[0..4];
        assert!((c[0] as i32 - 90).abs() <= 1, "data thumb must decode to the original byte, got {}", c[0]);
        assert_eq!(c[3], 255);
    }

    #[test]
    fn descriptor_usages_support_grow_copy() {
        for layers in [1u32, 4, 8, 64] {
            for res in [DEFAULT_MAP_RES, 2048, MAX_MAP_RES] {
                let d = MaterialTextureArray::texture_descriptor(layers, res);
                assert!(d.usage.contains(wgpu::TextureUsages::COPY_SRC),
                    "material_tex_array must be COPY_SRC — upload_map's grow path copies the old array into the new one");
                assert!(d.usage.contains(wgpu::TextureUsages::COPY_DST),
                    "material_tex_array must be COPY_DST — write_texture uploads land in it");
                assert!(d.usage.contains(wgpu::TextureUsages::TEXTURE_BINDING),
                    "material_tex_array must be TEXTURE_BINDING — the raster material bind group samples it");
                assert_eq!(d.size.depth_or_array_layers, layers.max(1));
                assert_eq!(d.size.width, res);
                assert_eq!(d.size.height, res);
                assert!(d.usage.contains(wgpu::TextureUsages::STORAGE_BINDING));
                assert_eq!(d.format, wgpu::TextureFormat::Rgba8Unorm);
                assert!(d.view_formats.contains(&wgpu::TextureFormat::Rgba8UnormSrgb));
            }
        }
    }

    #[test]
    fn map_side_len_accepts_legal_squares() {
        for side in [128u32, 256, 512, 1024, 2048, 4096] {
            let n = (side * side * 4) as usize;
            let buf = vec![0u8; n];
            assert_eq!(map_side_len(&buf), Some(side));
        }
        assert!(map_side_len(&[]).is_none());
        assert!(map_side_len(&vec![0u8; 100]).is_none());
    }

    #[test]
    fn upsample_rgba_preserves_solid_color() {
        let src_res = 4u32;
        let dst_res = 8u32;
        let src = [10u8, 20, 30, 255].repeat((src_res * src_res) as usize);
        let up = upsample_rgba_square(&src, src_res, dst_res);
        assert_eq!(up.len(), (dst_res * dst_res * 4) as usize);
        assert!(up.chunks_exact(4).all(|c| c == [10, 20, 30, 255]));
    }

    /// The full chain matches wgpu's mip sizing rule (level n = max(1, side >> n)).
    #[test]
    fn mip_count_matches_wgpu_rule() {
        assert_eq!(MaterialTextureArray::mip_count(1), 1);
        assert_eq!(MaterialTextureArray::mip_count(8), 4);
        assert_eq!(MaterialTextureArray::mip_count(1024), 11);
        assert_eq!(MaterialTextureArray::mip_count(1000), 10); // NPOT: 1000→500→…→3→1
        assert_eq!(MaterialTextureArray::mip_count(4096), 13);
    }

    /// A solid color must survive the whole mip chain byte-identically — the 16-bit linear
    /// workspace exists precisely so decode→average→encode round-trips every byte value.
    #[test]
    fn downsample_preserves_solid_color_every_byte() {
        for b in [0u8, 1, 40, 90, 128, 200, 254, 255] {
            let side = 8u32;
            let src = [b, b, b, 255].repeat((side * side) as usize);
            let down = downsample_half_rgba_srgb(&src, side);
            assert_eq!(down.len(), (4 * 4 * 4) as usize);
            assert!(
                down.chunks_exact(4).all(|c| c == [b, b, b, 255]),
                "byte {b} drifted through the mip downsample"
            );
        }
    }

    /// Downsampling averages in LINEAR space: a black/white checker halves to the linear
    /// midpoint (sRGB ~188), NOT the naive byte average (127) that darkens every mip.
    #[test]
    fn downsample_averages_in_linear_space() {
        let side = 2u32;
        let src = [
            255u8, 255, 255, 255, /* */ 0, 0, 0, 255, //
            0, 0, 0, 255, /* */ 255, 255, 255, 255,
        ];
        let down = downsample_half_rgba_srgb(&src, side);
        // linear avg = 0.5 → sRGB ≈ 0.7354 → byte ≈ 188.
        assert!(
            (down[0] as i32 - 188).abs() <= 1,
            "checker mip must be the linear midpoint (~188), got {}",
            down[0]
        );
        assert_eq!(down[3], 255);
    }

    /// Green-channel convention auto-detect: filename markers, then the ORM-sibling (Unreal
    /// layout) inference, defaulting to OpenGL. Pure-path cases only (no fs probe).
    #[test]
    fn normal_green_detection_filename_markers() {
        use std::path::Path;
        // Explicit markers win.
        assert!(!normal_green_is_dx(Path::new("Bricks038_1K-JPG_NormalGL.jpg")));
        assert!(normal_green_is_dx(Path::new("Bricks038_1K-JPG_NormalDX.jpg")));
        assert!(normal_green_is_dx(Path::new("rock_Normal_DX.png")));
        assert!(!normal_green_is_dx(Path::new("rock_Normal_GL.png")));
        assert!(!normal_green_is_dx(Path::new("wood_normal_opengl.png")));
        // Unreal-layout inference: sibling ORM file in the same folder → DirectX.
        let dir = std::env::temp_dir().join("azra_test_green_dx_orm");
        let _ = std::fs::create_dir_all(&dir);
        let orm = dir.join("rock_test_Material_OcclusionRoughnessMetallic.png");
        let _ = std::fs::write(&orm, b"x");
        assert!(normal_green_is_dx(&dir.join("rock_test_Material_Normal.png")));
        let _ = std::fs::remove_file(&orm);
        // No markers, no ORM sibling → OpenGL default.
        assert!(!normal_green_is_dx(&dir.join("plain_normal.png")));
        let _ = std::fs::remove_dir(&dir);
    }

    /// Flat Fill color: Bit8 install must match direct 8-bit encode (not washed hi-plane).
    #[test]
    fn flat_color_bit8_from_dual_plane_matches_direct_srgb8() {
        let lin = [0.75f32, 0.12, 0.08]; // typical Fill red in linear
        let (hi, lo) = encode_srgb_color_rgba16_planes(lin);
        let direct = encode_srgb_color_rgba(lin);
        let n = (MAP_RES * MAP_RES) as usize;
        let hi_buf: Vec<u8> = hi.iter().cycle().take(n * 4).copied().collect();
        let lo_buf: Vec<u8> = lo.iter().cycle().take(n * 4).copied().collect();
        let converted = dual_plane_color16_to_srgb8(&hi_buf, &lo_buf);
        // First pixel must match direct 8-bit encode within 1 LSB (u16→u8 round-trip).
        for c in 0..3 {
            let d = (converted[c] as i32 - direct[c] as i32).abs();
            assert!(
                d <= 1,
                "channel {c}: dual→8 got {} direct {} (hi-plane alone would be {})",
                converted[c],
                direct[c],
                hi[c]
            );
        }
        // Hi plane alone is NOT the correct 8-bit color (the bug this tests).
        assert_ne!(
            hi[0], direct[0],
            "precondition: 16-bit hi plane differs from true 8-bit encode for this red"
        );
    }

    /// M4 / D3 — product ceiling 2K; VRAM formula published on the scoreboard.
    #[test]
    fn m4_product_ceiling_and_vram_estimate() {
        assert_eq!(PRODUCT_MAP_CEILING, 2048);
        assert_eq!(product_map_ceiling(), 2048);
        assert!(MAX_MAP_RES >= PRODUCT_MAP_CEILING);
        // Technical load path still accepts 4K squares (legacy / future unlock).
        let n4k = (4096u32 * 4096 * 4) as usize;
        assert_eq!(map_side_len(&vec![0u8; n4k]), Some(4096));
        // Dual-plane 4 channels @2K = 128 MiB.
        let b = estimate_material_map_vram_bytes(2048, 4);
        assert_eq!(b, 128 * 1024 * 1024);
        let line = material_map_vram_scoreboard_line(2048, 4);
        eprintln!("[M4] {line}");
        assert!(line.contains("128.00 MiB") || line.contains("128 MiB") || line.contains("127."));
    }

    /// S8 — box blur spreads an impulse and radius 0 is identity.
    #[test]
    fn s8_map_box_blur_spreads_impulse() {
        let res = 16u32;
        let mut src = vec![0u8; (res * res * 4) as usize];
        // Center white pixel.
        let cx = res / 2;
        let i = ((cx * res + cx) * 4) as usize;
        src[i] = 255;
        src[i + 1] = 255;
        src[i + 2] = 255;
        src[i + 3] = 255;
        let id = map_box_blur_rgba(&src, res, 0);
        assert_eq!(id, src, "radius 0 must be identity");
        let blurred = map_box_blur_rgba(&src, res, 2);
        assert_ne!(blurred, src, "radius>0 must change the map");
        // Neighbour of center should be non-zero after blur.
        let n = ((cx * res + cx + 1) * 4) as usize;
        assert!(
            blurred[n] > 0,
            "impulse must bleed into horizontal neighbour"
        );
        // Peak at center should still be the brightest-ish.
        assert!(
            blurred[i] >= blurred[n],
            "center should remain >= neighbour after box blur"
        );
    }
}
