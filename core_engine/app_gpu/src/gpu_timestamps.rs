//! GPU timestamp queries for per-pass GPU timing.
//!
//! Requires `wgpu::Features::TIMESTAMP_QUERY`. When not available, all operations are no-ops.
//! Results arrive 1–2 frames late (async GPU readback).
//!
//! Only query slots that were actually written to (via `compute_pass_writes` /
//! `render_pass_writes`) are resolved. Resolving unwritten timestamp queries causes
//! device loss on Vulkan.
//!
//! **Resolve strategy:** each *active* pass slot is resolved as its own 2-query range into
//! a **256-byte-aligned** region of the resolve buffer (`QUERY_RESOLVE_BUFFER_ALIGNMENT`).
//! That way non-contiguous slots (deferred=1, tonemap=8, shadow=11, gbuf=12) all land without
//! requiring `TIMESTAMP_QUERY_INSIDE_ENCODERS` gap-fill, and without the old "first contiguous
//! run only" path that permanently zeroed `gpuSh`/`gpuGb` (G9).

use eframe::wgpu;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::sync::mpsc;

/// Maximum number of timestamp pairs (begin + end per pass).
/// 20 passes × 2 = 40 queries (full query set).
///
/// Raised from 16 on 2026-08-06: PASS_SHADOW timed cascade 0 ONLY, so the profiler
/// reported a THIRD of the real shadow cost and shadow-heavy frames looked cheap. At
/// 8192² that is a 3x under-report of the single most expensive pass in the frame.
/// Slots are resolved individually against `active_passes`, so unused ones stay
/// unwritten and never violate the Vulkan "don't resolve unwritten queries" rule.
const MAX_QUERIES: u32 = 40;
const MAX_PASSES: usize = MAX_QUERIES as usize / 2;

/// wgpu requires resolve destination offsets to be multiples of this (typically 256).
const RESOLVE_ALIGN: u64 = 256;
/// Bytes reserved per pass slot in the resolve/staging buffers (aligned region).
const SLOT_STRIDE: u64 = RESOLVE_ALIGN;
/// u64 elements per aligned slot region in the staging view.
const SLOT_U64_STRIDE: usize = (SLOT_STRIDE / 8) as usize;

/// Named GPU pass slot indices (each pass uses 2 query indices: begin, end).
pub const PASS_RAYMARCH: u32 = 0;
pub const PASS_DEFERRED: u32 = 1;
pub const PASS_BEVEL: u32 = 2;
pub const PASS_SSR: u32 = 3;
pub const PASS_FOG: u32 = 4;
pub const PASS_BLOOM_EXT: u32 = 5;
pub const PASS_BLOOM_H: u32 = 6;
pub const PASS_BLOOM_V: u32 = 7;
pub const PASS_TONEMAP: u32 = 8;
pub const PASS_UPSAMPLE: u32 = 9;
#[allow(dead_code)]
pub const PASS_FXAA: u32 = 10;
pub const PASS_SHADOW: u32 = 11;
pub const PASS_RASTER_GBUF: u32 = 12;
/// Screen-space GI. **Times the GATHER ONLY** — the two blur passes ride untimed,
/// so the HUD number is a LOWER BOUND on the chain's real cost. It cannot be one
/// slot for all three: a timestamp slot brackets a single compute pass, and the
/// three cannot be merged into one pass because wgpu's usage scope spans the whole
/// pass and the gather's storage-write target is the blur's sampled source
/// (exclusive-usage conflict). Gate 5 must therefore either publish the gather
/// number while stating the blurs are excluded, or spend two more slots.
/// Reused slot 13, which was `PASS_BAKE`: that
/// constant was dead (grep-verified — the bake systems own private query sets),
/// so SSGI needed no MAX_QUERIES bump and none of the silent-zero risk that
/// comes with one. Only written when the pass actually dispatches.
pub const PASS_SSGI: u32 = 13;
/// SSAO (main hemisphere pass — the blur pair rides untimed; the whole 3-pass
/// chain measures ~0.07 ms at 1080p, see render/src/ssao.rs timing test).
pub const PASS_SSAO: u32 = 14;
/// RT projected shadows (point/spot BVH visibility). Only written when the RT
/// pass actually dispatches — never resolve an unused slot (GPU Safety).
pub const PASS_RT_SHADOW: u32 = 15;
/// Sun CSM cascades 1 and 2. Cascade 0 is `PASS_SHADOW`; the profiler SUMS all three,
/// because one cascade is not "the shadow cost" — every cascade clears and rasterizes
/// the whole scene into its own full-resolution depth target every frame.
pub const PASS_SHADOW_1: u32 = 16;
pub const PASS_SHADOW_2: u32 = 17;
pub const PASS_PATH_TRACE: u32 = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuTimingScope {
    Pass,
    EncoderInterval,
}

#[must_use]
pub struct EncoderInterval<'timer> {
    timer: &'timer mut GpuTimestamps,
    encoder: &'timer mut wgpu::CommandEncoder,
    slot: u32,
}

impl EncoderInterval<'_> {
    pub fn encoder(&mut self) -> &mut wgpu::CommandEncoder {
        self.encoder
    }

    pub fn end_interval(self) {
        drop(self);
    }
}

impl Drop for EncoderInterval<'_> {
    fn drop(&mut self) {
        let mask = 1 << self.slot;
        assert!(self.timer.open_intervals & mask != 0);
        self.encoder.write_timestamp(&self.timer.query_set, self.slot * 2 + 1);
        self.timer.open_intervals &= !mask;
    }
}

/// GPU timestamp query system.
pub struct GpuTimestamps {
    pub query_set: wgpu::QuerySet,
    pub resolve_buffer: wgpu::Buffer,
    pub staging_buffer: wgpu::Buffer,
    /// Nanoseconds per GPU tick — from `queue.get_timestamp_period()`.
    pub timestamp_period: f32,
    /// True if a readback is in-flight and we should poll for results.
    pub pending_readback: bool,
    /// Set to true by the map_async callback when the readback completes, including failures.
    pub map_ready: Arc<AtomicBool>,
    /// Last resolved per-pass GPU times in milliseconds (indexed by PASS_* constants).
    pub pass_times_ms: [f32; MAX_PASSES],
    /// Bitmask of pass slots that were written this frame.
    /// Set by `compute_pass_writes()` / `render_pass_writes()`, consumed by `resolve_and_copy()`.
    active_passes: u32,
    encoder_intervals: u32,
    open_intervals: u32,
    /// Bitmask of slots that the in-flight (or just-completed) resolve covers.
    /// `poll_results` only updates these indices; others are zeroed.
    resolved_passes: u32,
    resolved_intervals: u32,
    completed_passes: u32,
    completed_intervals: u32,
    map_result: Option<mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>>,
    pub readback_error: Option<wgpu::BufferAsyncError>,
    /// Whether TIMESTAMP_QUERY_INSIDE_ENCODERS is available.
    can_write_timestamp: bool,
}

impl GpuTimestamps {
    /// Create the query system. `can_write_ts` should be true if
    /// TIMESTAMP_QUERY_INSIDE_ENCODERS is enabled on the device.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let can_write_ts = device.features().contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS);
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("gpu_timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: MAX_QUERIES,
        });
        // One 256-byte-aligned region per pass slot (2×u64 used at the head of each region).
        let buf_size = (MAX_PASSES as u64) * SLOT_STRIDE;
        let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gpu_ts_resolve"),
            size: buf_size,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gpu_ts_staging"),
            size: buf_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        GpuTimestamps {
            query_set,
            resolve_buffer,
            staging_buffer,
            timestamp_period: queue.get_timestamp_period(),
            pending_readback: false,
            map_ready: Arc::new(AtomicBool::new(false)),
            pass_times_ms: [0.0; MAX_PASSES],
            active_passes: 0,
            encoder_intervals: 0,
            open_intervals: 0,
            resolved_passes: 0,
            resolved_intervals: 0,
            completed_passes: 0,
            completed_intervals: 0,
            map_result: None,
            readback_error: None,
            can_write_timestamp: can_write_ts,
        }
    }

    /// Reset active pass tracking. Call **once** at the start of each frame's render
    /// loop — **before** any pass that writes timestamps (including the early hybrid
    /// raster/shadow encoder). Calling it after raster clears slots 11/12 and G9
    /// permanently reports `gpuSh=0 gpuGb=0`.
    pub fn begin_frame(&mut self) {
        assert_eq!(self.open_intervals, 0, "cannot reset an unclosed GPU timestamp interval");
        self.active_passes = 0;
        self.encoder_intervals = 0;
    }

    pub fn supports_encoder_intervals(&self) -> bool {
        self.can_write_timestamp
    }

    pub fn begin_interval<'timer>(
        &'timer mut self,
        encoder: &'timer mut wgpu::CommandEncoder,
        slot: u32,
    ) -> Option<EncoderInterval<'timer>> {
        assert!(slot < MAX_PASSES as u32);
        if !self.can_write_timestamp {
            return None;
        }
        let mask = 1 << slot;
        assert_eq!(self.active_passes & mask, 0, "GPU timestamp slot already written this frame");
        encoder.write_timestamp(&self.query_set, slot * 2);
        self.active_passes |= mask;
        self.encoder_intervals |= mask;
        self.open_intervals |= mask;
        Some(EncoderInterval { timer: self, encoder, slot })
    }

    pub fn time_encoder<Output>(
        &mut self,
        slot: u32,
        encoder: &mut wgpu::CommandEncoder,
        operation: impl FnOnce(&mut wgpu::CommandEncoder) -> Output,
    ) -> Output {
        assert!(slot < MAX_PASSES as u32);
        if self.can_write_timestamp {
            let mut interval = self.begin_interval(encoder, slot).expect("encoder timestamps enabled");
            operation(interval.encoder())
        } else {
            operation(encoder)
        }
    }

    /// Build `ComputePassTimestampWrites` for a given pass slot.
    /// Marks the slot as active so `resolve_and_copy` knows which queries to resolve.
    pub fn compute_pass_writes(&mut self, pass_slot: u32) -> wgpu::ComputePassTimestampWrites<'_> {
        assert!(pass_slot < MAX_PASSES as u32);
        assert_eq!(self.encoder_intervals & (1 << pass_slot), 0, "cannot overwrite encoder interval queries");
        self.active_passes |= 1 << pass_slot;
        wgpu::ComputePassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: Some(pass_slot * 2),
            end_of_pass_write_index: Some(pass_slot * 2 + 1),
        }
    }

    /// Build `RenderPassTimestampWrites` for a given pass slot.
    /// Marks the slot as active so `resolve_and_copy` knows which queries to resolve.
    pub fn render_pass_writes(&mut self, pass_slot: u32) -> wgpu::RenderPassTimestampWrites<'_> {
        assert!(pass_slot < MAX_PASSES as u32);
        assert_eq!(self.encoder_intervals & (1 << pass_slot), 0, "cannot overwrite encoder interval queries");
        self.active_passes |= 1 << pass_slot;
        wgpu::RenderPassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: Some(pass_slot * 2),
            end_of_pass_write_index: Some(pass_slot * 2 + 1),
        }
    }

    /// Sum of last resolved pass times (ms). Scoreboard `gpu_ms=` field.
    pub fn total_ms(&self) -> f32 {
        self.pass_times_ms.iter().sum()
    }

    pub fn completed_frame_ms(&self, required_pass: u32) -> Option<f32> {
        assert!(required_pass < MAX_PASSES as u32);
        if self.completed_passes & (1 << required_pass) == 0 {
            return None;
        }
        let total = self.total_ms();
        if total.is_finite() && total > 0.0 {
            Some(total)
        } else {
            None
        }
    }

    pub fn completed_timing_scope(&self, pass_slot: u32) -> Option<GpuTimingScope> {
        assert!(pass_slot < MAX_PASSES as u32);
        let mask = 1 << pass_slot;
        if self.completed_passes & mask == 0 {
            None
        } else if self.completed_intervals & mask != 0 {
            Some(GpuTimingScope::EncoderInterval)
        } else {
            Some(GpuTimingScope::Pass)
        }
    }

    /// After `queue.submit()`, resolve queries and copy to staging buffer.
    /// Resolves **each active slot independently** into a 256-aligned region so
    /// non-contiguous passes (shadow=11, gbuf=12 vs deferred=1) are never dropped.
    pub fn resolve_and_copy(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        assert_eq!(self.open_intervals, 0, "cannot resolve an unclosed GPU timestamp interval");
        if self.active_passes == 0 { return; } // Nothing to resolve

        // If a previous readback is still pending, the staging buffer is mapped.
        // Using a mapped buffer as copy destination causes device loss on Vulkan.
        // Skip this frame's resolve — we'll get results next frame.
        if self.pending_readback {
            // Try to consume the pending results first
            if self.map_ready.load(Ordering::SeqCst) {
                // Data is ready — consume it before re-using the buffer
                let _ = self.poll_results(device);
            } else {
                return; // Still waiting — skip this frame
            }
        }

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_ts_resolve"),
        });

        // Per-slot resolve into 256-aligned destinations. Never touch unwritten query
        // indices (Vulkan device-loss rule). No need for TIMESTAMP_QUERY_INSIDE_ENCODERS.
        for slot in 0..MAX_PASSES as u32 {
            if self.active_passes & (1 << slot) == 0 {
                continue;
            }
            let q0 = slot * 2;
            let dst_off = (slot as u64) * SLOT_STRIDE;
            debug_assert_eq!(dst_off % RESOLVE_ALIGN, 0);
            enc.resolve_query_set(
                &self.query_set,
                q0..q0 + 2,
                &self.resolve_buffer,
                dst_off,
            );
        }

        let copy_bytes = (MAX_PASSES as u64) * SLOT_STRIDE;
        enc.copy_buffer_to_buffer(
            &self.resolve_buffer, 0,
            &self.staging_buffer, 0,
            copy_bytes,
        );
        self.resolved_passes = self.active_passes;
        self.resolved_intervals = self.encoder_intervals;
        queue.submit(Some(enc.finish()));

        // Request async map of the full staging buffer (sparse slots stay unread garbage;
        // poll_results only interprets resolved_passes bits).
        let slice = self.staging_buffer.slice(..copy_bytes);
        self.map_ready.store(false, Ordering::SeqCst);
        let flag = self.map_ready.clone();
        let (sender, receiver) = mpsc::channel();
        self.map_result = Some(receiver);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
            flag.store(true, Ordering::SeqCst);
        });
        self.pending_readback = true;
    }

    /// Poll for readback results. Call once per frame at the start.
    /// Returns true if a fully valid completed timing frame was consumed.
    pub fn poll_results(&mut self, device: &wgpu::Device) -> bool {
        if !self.pending_readback { return false; }
        let _ = device.poll(wgpu::PollType::Poll);
        if !self.map_ready.load(Ordering::SeqCst) {
            return false; // Not ready yet
        }
        let result = match self.map_result.as_ref().expect("pending timestamp readback receiver").try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => panic!("timestamp readback callback disconnected"),
        };
        self.map_result = None;
        if let Err(error) = result {
            self.readback_error = Some(error);
            self.pass_times_ms.fill(0.0);
            self.completed_passes = 0;
            self.completed_intervals = 0;
            self.staging_buffer.unmap();
            self.pending_readback = false;
            return false;
        }
        let decoded = {
            let data = self.staging_buffer.slice(..).get_mapped_range();
            let timestamps: &[u64] = bytemuck::cast_slice(&data);
            decode_pass_times(timestamps, self.timestamp_period, self.resolved_passes)
        };
        self.pass_times_ms = decoded.unwrap_or([0.0; MAX_PASSES]);
        self.completed_passes = if decoded.is_some() { self.resolved_passes } else { 0 };
        self.completed_intervals = self.resolved_intervals & self.completed_passes;
        self.readback_error = None;
        self.staging_buffer.unmap();
        self.pending_readback = false;
        decoded.is_some()
    }
}

fn decode_pass_times(timestamps: &[u64], period: f32, mask: u32) -> Option<[f32; MAX_PASSES]> {
    if !period.is_finite() || period <= 0.0 {
        return None;
    }
    let mut times = [0.0; MAX_PASSES];
    for (slot, time) in times.iter_mut().enumerate() {
        if mask & (1 << slot) == 0 {
            continue;
        }
        let offset = slot * SLOT_U64_STRIDE;
        let begin = *timestamps.get(offset)?;
        let end = *timestamps.get(offset + 1)?;
        let ticks = end.checked_sub(begin)?;
        *time = (ticks as f64 * period as f64 / 1_000_000.0) as f32;
        if !time.is_finite() {
            return None;
        }
    }
    Some(times)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_trace_has_a_dedicated_in_bounds_slot() {
        assert_eq!(PASS_PATH_TRACE, 18);
        let slots = [
            PASS_RAYMARCH, PASS_DEFERRED, PASS_BEVEL, PASS_SSR, PASS_FOG,
            PASS_BLOOM_EXT, PASS_BLOOM_H, PASS_BLOOM_V, PASS_TONEMAP,
            PASS_UPSAMPLE, PASS_FXAA, PASS_SHADOW, PASS_RASTER_GBUF,
            PASS_SSGI, PASS_SSAO, PASS_RT_SHADOW, PASS_SHADOW_1, PASS_SHADOW_2,
            PASS_PATH_TRACE,
        ];
        for (index, slot) in slots.iter().enumerate() {
            assert!(*slot < MAX_PASSES as u32);
            assert!(!slots[..index].contains(slot));
        }
    }

    #[test]
    fn completed_times_sum_only_actual_sparse_passes() {
        let mut timestamps = vec![u64::MAX; MAX_PASSES * SLOT_U64_STRIDE];
        let trace_offset = PASS_PATH_TRACE as usize * SLOT_U64_STRIDE;
        timestamps[trace_offset] = 0;
        timestamps[trace_offset + 1] = 15_000_000;
        let raster_offset = PASS_RASTER_GBUF as usize * SLOT_U64_STRIDE;
        timestamps[raster_offset] = 2_000_000;
        timestamps[raster_offset + 1] = 4_500_000;
        let mask = (1 << PASS_PATH_TRACE) | (1 << PASS_RASTER_GBUF);
        let times = decode_pass_times(&timestamps, 1.0, mask).unwrap();
        assert_eq!(times[PASS_PATH_TRACE as usize], 15.0);
        assert_eq!(times[PASS_RASTER_GBUF as usize], 2.5);
        assert_eq!(times[PASS_RAYMARCH as usize], 0.0);
        assert_eq!(times.iter().sum::<f32>(), 17.5);
        let only_raster = decode_pass_times(&timestamps, 1.0, 1 << PASS_RASTER_GBUF).unwrap();
        assert_eq!(only_raster[PASS_PATH_TRACE as usize], 0.0);
    }

    #[test]
    fn invalid_readbacks_cannot_supply_budget_timings() {
        let mask = 1 << PASS_PATH_TRACE;
        let mut timestamps = vec![0; MAX_PASSES * SLOT_U64_STRIDE];
        let offset = PASS_PATH_TRACE as usize * SLOT_U64_STRIDE;
        timestamps[offset] = 2;
        timestamps[offset + 1] = 1;
        assert!(decode_pass_times(&timestamps, 1.0, mask).is_none());
        timestamps[offset] = 0;
        timestamps[offset + 1] = 1;
        assert!(decode_pass_times(&timestamps[..offset + 1], 1.0, mask).is_none());
        for period in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(decode_pass_times(&timestamps, period, mask).is_none());
        }
    }

    #[test]
    fn encoder_interval_closes_and_fallback_stays_pass_only() {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })) else {
            eprintln!("SKIP encoder interval GPU test: no adapter");
            return;
        };
        if !adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            eprintln!("SKIP encoder interval GPU test: no timestamp queries");
            return;
        }
        for interval_enabled in [false, true] {
            if interval_enabled && !adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS) {
                continue;
            }
            let mut features = wgpu::Features::TIMESTAMP_QUERY;
            if interval_enabled {
                features |= wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
            }
            let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })).expect("timestamp interval fresh adapter");
            let (device, queue) = pollster::block_on(adapter.request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("encoder_interval_gpu_test"),
                    required_features: features,
                    required_limits: adapter.limits(),
                    memory_hints: wgpu::MemoryHints::default(),
                    trace: wgpu::Trace::Off,
                },
            )).expect("timestamp GPU test device");
            device.on_uncaptured_error(Box::new(|error| panic!("timestamp GPU test: {error}")));
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("timestamp_interval_workload"),
                source: wgpu::ShaderSource::Wgsl("@group(0) @binding(0) var<storage, read_write> values: array<u32>; @compute @workgroup_size(64) fn main(@builtin(global_invocation_id) gid: vec3<u32>) { values[gid.x] = gid.x; }".into()),
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: None,
                layout: None,
                module: &shader,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            });
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 256,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let copied = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 256,
                usage: wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &pipeline.get_bind_group_layout(0),
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
            });
            let mut timer = GpuTimestamps::new(&device, &queue);
            let mut encoder = device.create_command_encoder(&Default::default());
            timer.begin_frame();
            assert_eq!(timer.supports_encoder_intervals(), interval_enabled);
            if interval_enabled {
                let output = timer.time_encoder(PASS_PATH_TRACE, &mut encoder, |encoder| {
                    encoder.clear_buffer(&buffer, 0, None);
                    {
                        let mut pass = encoder.begin_compute_pass(&Default::default());
                        pass.set_pipeline(&pipeline);
                        pass.set_bind_group(0, &binding, &[]);
                        pass.dispatch_workgroups(1, 1, 1);
                    }
                    encoder.copy_buffer_to_buffer(&buffer, 0, &copied, 0, 256);
                    17
                });
                assert_eq!(output, 17);
                assert_eq!(timer.open_intervals, 0);
                assert_eq!(timer.encoder_intervals, 1 << PASS_PATH_TRACE);
            } else {
                assert!(timer.begin_interval(&mut encoder, PASS_PATH_TRACE).is_none());
                assert_eq!(timer.active_passes, 0);
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: None,
                    timestamp_writes: Some(timer.compute_pass_writes(PASS_PATH_TRACE)),
                });
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &binding, &[]);
                pass.dispatch_workgroups(1, 1, 1);
            }
            assert_eq!(timer.active_passes, 1 << PASS_PATH_TRACE);
            queue.submit([encoder.finish()]);
            timer.resolve_and_copy(&device, &queue);
            device.poll(wgpu::PollType::Wait).expect("timestamp interval GPU poll");
            assert!(timer.poll_results(&device));
            assert_eq!(timer.completed_timing_scope(PASS_PATH_TRACE), Some(if interval_enabled {
                GpuTimingScope::EncoderInterval
            } else {
                GpuTimingScope::Pass
            }));
            assert_eq!(timer.completed_timing_scope(PASS_RAYMARCH), None);
            assert!(!timer.pending_readback);
        }
    }
}
