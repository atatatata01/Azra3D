// depth_probe.rs — read a handful of texels back out of the raster depth buffer.
//
// WHY THIS EXISTS (VIEWPORT_PERF_2026-08-12 item A). Viewport overlays that draw a world-space
// marker — a light icon, and anything else that must hide behind geometry — need one answer:
// "is this world point behind the surface the user can see?" That is a DEPTH test, and the raster
// pass already computed it for every pixel on the GPU. Asking it on the CPU instead, by shooting
// a ray through every triangle in the scene, cost **31 ms of a 33 ms frame** for ONE light on a
// 323k-tri body — the cost scaled with TRIANGLES, not with the number of markers, which is why a
// scene reading `Lights: 1` in the HUD was spending its whole frame here.
//
// So: sample the depth buffer at the marker's own pixel and compare. A few texels per frame,
// O(markers), independent of scene complexity.
//
// CONTRACT (mirrors `picking.rs`, whose crash history this module deliberately inherits):
// - ONE readback in flight. The caller consumes the previous result BEFORE issuing the next copy,
//   so the staging buffer is never mapped across a `Queue::submit` (the "still mapped" panic
//   class, picking.rs 2026-08-10).
// - `map_async`'s `Result` is never discarded — a failed map is recorded as a distinct state, not
//   left indistinguishable from "still pending" (a pending-forever probe would freeze every
//   marker's visibility for the session).
// - Results are ONE FRAME LATE by construction. That is correct for markers: an icon that
//   un-hides a frame after the surface moved off it is invisible to the user, and the alternative
//   (blocking the frame on a map) is the stall this module exists to remove.

/// Bytes reserved per probe. `copy_texture_to_buffer` requires each destination row to start at a
/// `COPY_BYTES_PER_ROW_ALIGNMENT` boundary, so each 1×1 texel copy gets its own aligned slot.
const SLOT_BYTES: u64 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64;

/// A point to sample: integer texel coordinates in the depth texture.
#[derive(Clone, Copy, Debug)]
pub struct ProbePoint {
    pub x: u32,
    pub y: u32,
}

/// Map states — three, not a bool. A bool is only ever set on success, so a FAILED map reads as
/// "still pending" forever and wedges the single-flight slot (picking.rs learned this the hard way).
pub const MAP_PENDING: u8 = 0;
pub const MAP_OK: u8 = 1;
pub const MAP_FAILED: u8 = 2;

/// Staging + single-flight state for depth texel readback.
pub struct DepthProbe {
    staging: Option<wgpu::Buffer>,
    /// Number of probe slots the staging buffer can hold.
    capacity: usize,
    /// Slots actually copied by the in-flight readback (`None` = nothing in flight).
    in_flight: Option<usize>,
    map_state: Option<std::sync::Arc<std::sync::atomic::AtomicU8>>,
}

impl Default for DepthProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl DepthProbe {
    pub fn new() -> Self {
        Self { staging: None, capacity: 0, in_flight: None, map_state: None }
    }

    /// True while a readback is in flight. The caller MUST NOT issue another copy until
    /// [`Self::try_take`] has returned — single-flight is what keeps the buffer unmapped
    /// across submits.
    pub fn is_pending(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Grow the staging buffer to hold `n` probes. Never called while a readback is in flight
    /// (single-flight, enforced by the caller) — replacing a mapped buffer is the zombie-buffer
    /// crash this contract exists to prevent.
    fn ensure_capacity(&mut self, device: &wgpu::Device, n: usize) {
        debug_assert!(!self.is_pending(), "DepthProbe: resized while a readback was in flight");
        if self.capacity >= n && self.staging.is_some() {
            return;
        }
        self.staging = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("depth_probe_staging"),
            size: SLOT_BYTES * n.max(1) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.capacity = n.max(1);
    }

    /// Encode one 1×1 depth copy per point (into `enc`, before its submit) and arm the readback.
    ///
    /// `depth` must be the `Depth32Float` raster depth texture, created with `COPY_SRC`.
    /// Points outside `(depth_w, depth_h)` are skipped — a marker off-screen has nothing to test
    /// against, and a clamped copy would silently answer for the wrong pixel.
    /// Returns the number of slots armed, in the same order as the surviving points.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        enc: &mut wgpu::CommandEncoder,
        depth: &wgpu::Texture,
        depth_w: u32,
        depth_h: u32,
        points: &[ProbePoint],
    ) -> usize {
        if self.is_pending() || points.is_empty() {
            return 0;
        }
        self.ensure_capacity(device, points.len());
        let Some(staging) = self.staging.as_ref() else { return 0 };
        let mut slot = 0usize;
        for p in points {
            if p.x >= depth_w || p.y >= depth_h {
                continue;
            }
            enc.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: depth,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: p.x, y: p.y, z: 0 },
                    aspect: wgpu::TextureAspect::DepthOnly,
                },
                wgpu::ImageCopyBuffer {
                    buffer: staging,
                    layout: wgpu::ImageDataLayout {
                        offset: SLOT_BYTES * slot as u64,
                        bytes_per_row: Some(SLOT_BYTES as u32),
                        rows_per_image: Some(1),
                    },
                },
                wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            );
            slot += 1;
        }
        if slot == 0 {
            return 0;
        }
        self.in_flight = Some(slot);
        slot
    }

    /// Start the async map. Call AFTER the encoder carrying [`Self::encode`]'s copies is submitted.
    pub fn start_readback(&mut self) {
        if self.in_flight.is_none() {
            return;
        }
        let Some(staging) = self.staging.as_ref() else {
            self.in_flight = None;
            return;
        };
        let state = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(MAP_PENDING));
        let flag = state.clone();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            // Record BOTH outcomes — a discarded Err is what wedges the single-flight slot.
            let v = if result.is_ok() { MAP_OK } else { MAP_FAILED };
            flag.store(v, std::sync::atomic::Ordering::SeqCst);
        });
        self.map_state = Some(state);
    }

    /// Non-blocking: when the map has resolved, decode the depth values, **unmap**, and free the
    /// single-flight slot. `None` while still pending.
    ///
    /// A FAILED map returns `Some(vec![])` rather than `None`: the slot must be released, and an
    /// empty result reads downstream as "no depth answer this frame" — the same thing the caller
    /// does before the first probe ever completes.
    pub fn try_take(&mut self, device: &wgpu::Device) -> Option<Vec<f32>> {
        let n = self.in_flight?;
        let state = self.map_state.clone()?;
        device.poll(wgpu::PollType::Poll).expect("GPU polling failed");
        match state.load(std::sync::atomic::Ordering::SeqCst) {
            MAP_PENDING => return None,
            MAP_FAILED => {
                // Never mapped — nothing to unmap, nothing to decode. Release the slot.
                self.in_flight = None;
                self.map_state = None;
                return Some(Vec::new());
            }
            _ => {}
        }
        let staging = self.staging.as_ref()?;
        let mut out = Vec::with_capacity(n);
        {
            let view = staging.slice(..).get_mapped_range();
            for i in 0..n {
                let off = (SLOT_BYTES as usize) * i;
                if off + 4 > view.len() {
                    break;
                }
                out.push(f32::from_le_bytes([
                    view[off], view[off + 1], view[off + 2], view[off + 3],
                ]));
            }
        }
        staging.unmap();
        self.in_flight = None;
        self.map_state = None;
        Some(out)
    }
}
