//! Phase D4 — cluster-cut GPU system (PHASE_D4_CLUSTER_RASTER_PLAN.md).
//!
//! Mirrors `InstanceCullSystem`'s shape: fixed regions in one args buffer,
//! 256-B param slots (one uniform written between dispatches of one encoder
//! would be clobbered), generation-keyed cached bind groups, non-blocking
//! counter readback. One dispatch per (clustered cid, pass); D4 caps the
//! clustered-cid count and instances-per-cid — streaming lifts both in D7.

use crate::cluster_gpu::ClusterGpuPool;
use core_math::scene_graph::MeshCacheId;

/// Args slots per (cid, pass) region — unselected slots self-skip
/// (index_count=0).
///
/// **16k was too small for AZRA_DENSE_STRESS=1x9 close-up** (~90k DAG clusters;
/// near-camera L0 cut can exceed 16k → silent drop → sparse fragment holes that
/// read as heavy flicker). 64k covers measured dense close-ups; overflow still
/// forces MESH (WP11). Memory: N_REGIONS × cap × 20 B ≈ 52 MB args buffer.
pub const MAX_VISIBLE_CLUSTERS_PER_CID: u32 = 65_536;
/// Simultaneously clustered meshes (D7 streaming lifts this).
pub const MAX_CLUSTERED_CIDS: usize = 8;
/// camera-early + 3 shadow cascades + camera-late (the D2 region pattern).
pub const CUT_PASSES: usize = 5;
/// Instances sharing one clustered cid per dispatch (vec4 slot limit in params).
pub const MAX_INSTANCES_PER_CID: usize = 4;

const PARAM_SLOT: u64 = 256;
const ARGS_STRIDE: u64 = 20;
const N_REGIONS: usize = MAX_CLUSTERED_CIDS * CUT_PASSES;
/// Counters: 8 stat slots + one region counter per region.
pub const COUNTER_WORDS: usize = 8 + N_REGIONS;

/// Mirrors `CutParams` in cluster_cut.wgsl (160 B ≤ 256 slot).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CutParamsGpu {
    pub planes: [[f32; 4]; 6],
    /// xyz camera position, w tan(fov_y/2).
    pub cam: [f32; 4],
    /// viewport_h, threshold_px, bitcast n_clusters, bitcast n_instances.
    pub metrics: [f32; 4],
    pub instance_slots: [u32; 4],
    /// args_base, counter index, passthrough, cap.
    pub region: [u32; 4],
    /// x = skip_cone (shadow passes), y/z/w reserved.
    pub flags: [u32; 4],
}

pub struct ClusterCutSystem {
    pub pipeline_reset: wgpu::ComputePipeline,
    pub pipeline_cut: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    /// N_REGIONS × 256-B param slots (indexed by region id = cid_slot * CUT_PASSES + pass).
    pub params_buffer: wgpu::Buffer,
    /// N_REGIONS × MAX_VISIBLE_CLUSTERS_PER_CID DrawIndexedIndirect args.
    pub args_buffer: wgpu::Buffer,
    /// COUNTER_WORDS u32s, zeroed by write_buffer each frame pre-submit.
    pub counters_buffer: wgpu::Buffer,
    /// Commit 7: per-region draw counts for multi_draw_indexed_indirect_count
    /// (copied from the region counters post-cut). INDIRECT|COPY_DST.
    pub count_buffer: wgpu::Buffer,
    /// True when MULTI_DRAW_INDIRECT_COUNT is enabled on the device — the encode
    /// uses the count path; false → fixed-cap self-skip fallback.
    pub use_count: bool,
    /// AZRA_FLICKER_WATCH: per-frame readback of one args region so the drawn
    /// cluster SET can be compared frame to frame (a constant COUNT does not prove
    /// a constant set). Diagnostic only — one 320 KB copy, ≤1 in flight.
    args_staging: wgpu::Buffer,
    args_rx: Option<std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>>,
    /// (order-independent set hash, entry count) of the last successful readback.
    pub last_args_hash: (u64, u32),
    /// Geometry-only hash (index_count + first_index), ignoring first_instance.
    pub last_geom_hash: u64,
    pub geom_changes: u32,
    /// How many times the set changed since the watch started, and total samples.
    pub args_changes: u32,
    pub args_samples: u32,
    counter_staging: wgpu::Buffer,
    counter_rx: Option<std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>>,
    pub last_counters: Vec<u32>,
    /// (pool generation, instance buffer generation) → per-(cid_slot, pass) bind
    /// groups for the cids resident at build time.
    cached: Option<(u64, u64, std::collections::HashMap<MeshCacheId, Vec<wgpu::BindGroup>>)>,
}

impl ClusterCutSystem {
    pub fn new(
        device: &wgpu::Device,
        bgl_entries: &[wgpu::BindGroupLayoutEntry],
        wgsl: &str,
    ) -> Self {
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cluster_cut_bgl"),
            entries: bgl_entries,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cluster_cut"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cluster_cut_pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let mk = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Self {
            pipeline_reset: mk("cs_cluster_reset"),
            pipeline_cut: mk("cs_cluster_cut"),
            bgl,
            params_buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cluster_cut_params"),
                size: N_REGIONS as u64 * PARAM_SLOT,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            args_buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cluster_cut_args"),
                size: N_REGIONS as u64 * MAX_VISIBLE_CLUSTERS_PER_CID as u64 * ARGS_STRIDE,
                // COPY_SRC: the AZRA_FLICKER_WATCH / CUT_DUMP readbacks. Without it
                // the copy is a validation error and silently yields zeros — which
                // is exactly what made both diagnostics report "0 clusters drawn"
                // while the counters said thousands.
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::INDIRECT
                    | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            counters_buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cluster_cut_counters"),
                size: (COUNTER_WORDS * 4) as u64,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            count_buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cluster_cut_counts"),
                size: (N_REGIONS * 4) as u64,
                usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            use_count: device
                .features()
                .contains(wgpu::Features::MULTI_DRAW_INDIRECT_COUNT),
            args_staging: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cluster_cut_args_staging"),
                size: MAX_VISIBLE_CLUSTERS_PER_CID as u64 * ARGS_STRIDE,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            args_rx: None,
            last_args_hash: (0, 0),
            last_geom_hash: 0,
            geom_changes: 0,
            args_changes: 0,
            args_samples: 0,
            counter_staging: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cluster_cut_counter_staging"),
                size: (COUNTER_WORDS * 4) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            counter_rx: None,
            last_counters: vec![0; COUNTER_WORDS],
            cached: None,
        }
    }

    /// Region id for (cid slot, pass).
    pub fn region_id(cid_slot: usize, pass: usize) -> u32 {
        (cid_slot * CUT_PASSES + pass) as u32
    }
    /// First args SLOT of a region (matches `CutParamsGpu.region[0]`).
    pub fn args_base(region: u32) -> u32 {
        region * MAX_VISIBLE_CLUSTERS_PER_CID
    }
    /// Byte offset of a region's args for the multi-draw encode.
    pub fn args_byte_offset(region: u32) -> u64 {
        Self::args_base(region) as u64 * ARGS_STRIDE
    }

    /// Write a region's params (pre-submit; applied before the encoder runs).
    pub fn write_region_params(&self, queue: &wgpu::Queue, region: u32, mut p: CutParamsGpu) {
        p.region[0] = Self::args_base(region);
        p.region[1] = region;
        p.region[3] = MAX_VISIBLE_CLUSTERS_PER_CID;
        queue.write_buffer(&self.params_buffer, region as u64 * PARAM_SLOT, bytemuck::bytes_of(&p));
    }

    /// Zero all counters for this frame (write_buffer applies pre-submit).
    pub fn zero_counters(&self, queue: &wgpu::Queue) {
        queue.write_buffer(&self.counters_buffer, 0, &vec![0u8; COUNTER_WORDS * 4]);
    }

    /// Ensure per-(resident cid, pass) bind groups match current generations.
    pub fn ensure_bind_groups(
        &mut self,
        device: &wgpu::Device,
        pool: &ClusterGpuPool,
        instance_buffer: &wgpu::Buffer,
        instance_buffer_generation: u64,
    ) {
        let key_ok = matches!(
            &self.cached,
            Some((pg, ig, _)) if *pg == pool.generation && *ig == instance_buffer_generation
        );
        if key_ok {
            return;
        }
        let mut map = std::collections::HashMap::new();
        for (slot, (cid, entry)) in Self::stable_cid_slots(pool) {
            let mut per_pass = Vec::with_capacity(CUT_PASSES);
            for pass in 0..CUT_PASSES {
                let region = Self::region_id(slot, pass);
                per_pass.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(&format!("cluster_cut_bg_{cid:?}_p{pass}")),
                    layout: &self.bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &self.params_buffer,
                                offset: region as u64 * PARAM_SLOT,
                                size: wgpu::BufferSize::new(std::mem::size_of::<CutParamsGpu>() as u64),
                            }),
                        },
                        wgpu::BindGroupEntry { binding: 1, resource: instance_buffer.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: entry.cluster_buffer.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: self.args_buffer.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 4, resource: self.counters_buffer.as_entire_binding() },
                    ],
                }));
            }
            map.insert(cid, per_pass);
        }
        self.cached = Some((pool.generation, instance_buffer_generation, map));
    }

    /// Deterministic cid → slot assignment (sorted by cid id; stable across
    /// frames while residency is unchanged). Capped at MAX_CLUSTERED_CIDS.
    pub fn stable_cid_slots(
        pool: &ClusterGpuPool,
    ) -> impl Iterator<Item = (usize, (MeshCacheId, &crate::cluster_gpu::ClusterCidEntry))> {
        let mut cids: Vec<_> = pool.entries.iter().collect();
        cids.sort_unstable_by_key(|(c, _)| c.0);
        cids.truncate(MAX_CLUSTERED_CIDS);
        cids.into_iter().enumerate().map(|(i, (c, e))| (i, (*c, e)))
    }

    pub fn pass_bind_group(&self, cid: MeshCacheId, pass: usize) -> Option<&wgpu::BindGroup> {
        self.cached.as_ref()?.2.get(&cid)?.get(pass)
    }

    /// Commit 7: copy the region counters into the INDIRECT count buffer (encode
    /// AFTER the cut compute pass, BEFORE any pass that multi-draws with count).
    /// Counts may exceed CAP on overflow — `_count` clamps to max_count (CAP).
    pub fn encode_count_copy(&self, enc: &mut wgpu::CommandEncoder) {
        enc.copy_buffer_to_buffer(
            &self.counters_buffer,
            8 * 4, // region counters start after the 8 stat slots
            &self.count_buffer,
            0,
            (N_REGIONS * 4) as u64,
        );
    }

    /// Byte offset of a region's u32 in `count_buffer`.
    pub fn count_byte_offset(region: u32) -> u64 {
        region as u64 * 4
    }

    /// AZRA_FLICKER_WATCH: copy one args region for frame-to-frame set comparison.
    pub fn encode_args_copy(&self, enc: &mut wgpu::CommandEncoder, region: u32) {
        if self.args_rx.is_none() {
            enc.copy_buffer_to_buffer(
                &self.args_buffer,
                Self::args_byte_offset(region),
                &self.args_staging,
                0,
                MAX_VISIBLE_CLUSTERS_PER_CID as u64 * ARGS_STRIDE,
            );
        }
    }

    /// Drain the args readback and compare the drawn SET with the previous sample.
    /// The hash is ORDER-INDEPENDENT (xor+add of per-entry hashes) because the cut
    /// appends via atomicAdd — slot order legitimately varies, membership must not.
    /// Returns Some((changed, count)) on a fresh sample.
    pub fn poll_args_hash(&mut self) -> Option<(bool, u32)> {
        if let Some(rx) = &self.args_rx {
            match rx.try_recv() {
                Ok(Ok(())) => {
                    let (mut acc_x, mut acc_s, mut n) = (0u64, 0u64, 0u32);
                    // Split hash: GEOMETRY identity (which clusters) vs the full
                    // record (which also carries first_instance). If geometry is
                    // stable but the full hash moves, the instance pointer is the
                    // thing changing, not the selection.
                    let (mut geom_x, mut geom_s) = (0u64, 0u64);
                    {
                        let data = self.args_staging.slice(..).get_mapped_range();
                        let words: &[u32] = bytemuck::cast_slice(&data);
                        for a in words.chunks_exact(5) {
                            if a[0] == 0 {
                                continue; // self-skip slot
                            }
                            let mut h = 0xcbf29ce484222325u64;
                            for &w in a {
                                h = (h ^ w as u64).wrapping_mul(0x100000001b3);
                            }
                            // Two INDEPENDENT commutative accumulators. Mixing xor
                            // and add into ONE running value is order-DEPENDENT, which
                            // made this report changes for a set that only differed in
                            // slot order (atomicAdd append order legitimately varies).
                            acc_x ^= h;
                            acc_s = acc_s.wrapping_add(h);
                            // geometry-only: index_count + first_index
                            let mut g = 0xcbf29ce484222325u64;
                            for w in [a[0], a[2]] {
                                g = (g ^ w as u64).wrapping_mul(0x100000001b3);
                            }
                            geom_x ^= g;
                            geom_s = geom_s.wrapping_add(g);
                            n += 1;
                        }
                    }
                    let geom = geom_x ^ geom_s.rotate_left(31);
                    let acc = acc_x ^ acc_s.rotate_left(31);
                    self.args_staging.unmap();
                    self.args_rx = None;
                    let prev = self.last_args_hash;
                    let prev_geom = self.last_geom_hash; // read BEFORE overwrite
                    self.last_args_hash = (acc, n);
                    self.last_geom_hash = geom;
                    self.args_samples += 1;
                    // First sample has no predecessor.
                    let changed = prev != (0, 0) && prev != (acc, n);
                    if changed {
                        self.args_changes += 1;
                    }
                    if prev_geom != 0 && prev_geom != geom {
                        self.geom_changes += 1;
                    }
                    return Some((changed, n));
                }
                Ok(Err(_)) => self.args_rx = None,
                Err(_) => {}
            }
        } else {
            let (tx, rx) = std::sync::mpsc::channel();
            self.args_staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
            self.args_rx = Some(rx);
        }
        None
    }

    /// Encode counters → staging (pre-submit, ≤1 in flight).
    pub fn encode_counter_copy(&self, enc: &mut wgpu::CommandEncoder) {
        if self.counter_rx.is_none() {
            enc.copy_buffer_to_buffer(
                &self.counters_buffer,
                0,
                &self.counter_staging,
                0,
                (COUNTER_WORDS * 4) as u64,
            );
        }
    }

    /// Post-submit non-blocking readback (1–2 frame lag — scoreboard use).
    pub fn poll_counters(&mut self) {
        if let Some(rx) = &self.counter_rx {
            match rx.try_recv() {
                Ok(Ok(())) => {
                    {
                        let data = self.counter_staging.slice(..).get_mapped_range();
                        let vals: &[u32] = bytemuck::cast_slice(&data);
                        self.last_counters.copy_from_slice(&vals[..COUNTER_WORDS]);
                    }
                    self.counter_staging.unmap();
                    self.counter_rx = None;
                }
                Ok(Err(_)) => self.counter_rx = None,
                Err(_) => {}
            }
        } else {
            let (tx, rx) = std::sync::mpsc::channel();
            self.counter_staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
            self.counter_rx = Some(rx);
        }
    }
}
