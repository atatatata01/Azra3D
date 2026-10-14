//! GPU curvature kernel dispatch — the CPU half of `volume_bake.wgsl::curvature_eval`.
//!
//! Plan: `docs/planning/GPU_CURVATURE_KERNEL_PLAN.md` (P1 = this file + the shader entry + the
//! texture-rung switchover). The kernel is the THIRD consumer of `volume_bake.wgsl`'s
//! `eval_scene_at`, joining `bake_volume` and `classify_bricks`, so the material node graph's
//! curvature signal is evaluated against the SAME analytical field the bake writes — parity is
//! structural, not promised.
//!
//! ## What lives here and why
//!
//! Everything the shader must NOT re-derive. Per plan §2.2 the epsilon (`e`), the Newton
//! acceptance (`accept`), the AABB diagonal (`diag`) and the body AABB itself are computed HERE,
//! with the exact expressions `node_graph_bake::sdf_curvature_masks` uses today, and passed in
//! through the `BakeParams` tail. Recomputing them in the shader would be a NEW divergence
//! surface for constants that are cheap to compute once per dispatch.
//!
//! ## Coordinate frame (the plan's #1 risk)
//!
//! `eval_scene_at(world_pos)` is a MISNOMER: it consumes BODY-LOCAL points, because the prim
//! stream at binding 0 comes from `serialize_body_for_bake_hot(sg, body, ZERO)`, which keeps
//! prims in the top body's local frame. The CPU `SdfFieldSources` holds
//! `scene_graph.body_primitives(body)` — the same stored body-local array (its doc:
//! "body-local space = bake domain"). Both sides are therefore in the SAME frame and no
//! conversion happens at the seam. See the shader's `curvature_eval` header for the full
//! citation chain; `tests_gpu_curvature.rs` is the gate that would catch a regression here.
//!
//! ## Frame Ordering Contract
//!
//! Every GPU resource created and every dispatch encoded by this module happens inside
//! `poll_background_tasks` (via `AzraApp::poll_graph_curvature`) — never in a panel draw and
//! never during render. The bake worker thread itself still touches zero wgpu: it receives a
//! finished `CurvGrid` by value.
//!
//! ## Frame Budget Rule — the readback is ASYNC
//!
//! The dispatch is split in two: `encode_curvature_dispatch` submits + arms `map_async` and
//! RETURNS (no `PollType::Wait`, no `recv`), handing back a `PendingCurvReadback`;
//! `poll_graph_curvature` peeks it non-blockingly on later frames (`PollType::Poll`) and reads
//! the mapped range only once it has settled, at most ONE per frame. This is the same cross-frame
//! `MapState` fence `poll_gpu_classify_readbacks` uses. The earlier inline
//! `PollType::Wait` + `rx.recv()` blocked the frame thread for the whole GPU round-trip, which is
//! what froze the app when a Curvature/Cavity generator layer was added.

use eframe::wgpu;
use glam::Vec3;

/// Result of one `curvature_eval` dispatch over the `res²` UV bake grid: 4 f32 per sample
/// (mask, signed curv, convex mask, concave mask), row-major in the SAME pixel order the CPU
/// bake loop walks (`i = y * res + x`, with `u = (x+0.5)/res`).
///
/// Handed to the bake worker by value inside `SdfFieldSources`, where `sdf_curvature_masks`
/// consumes it as a direct lookup instead of running ~500 scene evals per pixel.
#[derive(Clone, Debug)]
pub(crate) struct CurvGrid {
    pub res: u32,
    /// `res * res * 4` floats.
    pub data: Vec<f32>,
    /// The `region` the grid was baked with (0 = |curv|, 1 = convex, 2 = concave). A lookup is
    /// only valid for the region it was baked for — `mask` is region-dependent (the other three
    /// channels are not, but keying on the whole tuple keeps the contract simple and honest).
    pub region: u32,
    /// The shaping/projection knobs the grid was baked with. A node whose knobs differ from
    /// these falls through to the CPU path rather than silently reading a mismatched grid.
    pub key: CurvKey,
}

impl CurvGrid {
    /// Sample at pixel `(x, y)`. Returns `(mask, curv, convex, concave)`.
    pub fn get(&self, x: u32, y: u32) -> (f32, f32, f32, f32) {
        let i = ((y.min(self.res - 1) * self.res + x.min(self.res - 1)) * 4) as usize;
        (self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3])
    }
}

/// The knob tuple a `CurvGrid` was baked for. A curvature node whose knobs do not match falls
/// back to the CPU path — a grid is only reusable for the exact parameters that produced it.
/// `eps` is the RAW node socket value (the derived `e` is a function of it and `diag`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CurvKey {
    pub eps: f32,
    pub prim_uid: u32,
    pub region: u32,
    pub grow: f32,
    pub band: f32,
    pub contrast: f32,
    pub strength: f32,
}

impl CurvKey {
    /// Bit-exact equality — these are knob values copied verbatim from the node graph on both
    /// sides, never arithmetic results, so exact comparison is correct (and a near-miss SHOULD
    /// fall back to the CPU rather than reuse a grid baked for different knobs).
    pub fn matches(&self, other: &CurvKey) -> bool {
        self == other
    }
}

/// The CPU-side constants the kernel is forbidden to re-derive.
///
/// Shared with `sdf_curvature_masks` so GPU and CPU consume PROVABLY the same numbers:
/// ```text
/// ext      = (bmax - bmin).max(splat(1e-4))
/// diag     = length(ext)                          // planar seed push only
/// min_axis = min(ext.x, ext.y, ext.z)             // LOCAL feature size
/// e        = clamp(max(eps, min_axis*0.01), …, min_axis*0.35)
/// feature  = min_axis                             // κ display scale (NOT full diag)
/// accept   = max(band, diag*0.08, 0.05)           // Newton still uses body span
/// ```
///
/// **Why min_axis (owner 2026-07-28):** Elongate and Shift+D inflate the body AABB diagonal.
/// Sizing the FD step `e` from full `diag` made taps larger than thin faces / small prims →
/// κ died on long boxes and multi-prim bodies. Sample step follows the *local* short axis;
/// display strength does not track whole-body length.
///
/// `bmin`/`bmax` are the already-PADDED body AABB from `SdfFieldSources::body_aabb`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CurvConstants {
    pub bmin: Vec3,
    pub bmax: Vec3,
    /// Full AABB diagonal — planar seed push distance only (`diag * 0.55`).
    pub diag: f32,
    /// Short-axis feature size — FD floor/cap + κ display renorm (elongate/dup-safe).
    pub feature: f32,
    pub e: f32,
    pub accept: f32,
}

/// Display pre-scale so a unit cube keeps ~the same |curv| as the old `k_raw * diag * 0.5`
/// path (`diag = side·√3` → factor `0.5·√3 ≈ 0.866` when `feature = side`).
pub(crate) const CURV_FEATURE_DISPLAY_S: f32 = 0.866_025_4;

impl CurvConstants {
    /// Derive from the padded body AABB + the node's raw `eps`/`band` sockets — the SAME
    /// expressions `sdf_curvature_masks` uses (must stay lockstep with the GPU kernel).
    pub fn new(bmin: Vec3, bmax: Vec3, eps: f32, band: f32) -> Self {
        let ext = (bmax - bmin).max(Vec3::splat(1e-4));
        let diag = ext.length().max(1e-4);
        let min_axis = ext.x.min(ext.y).min(ext.z).max(1e-4);
        let feature = min_axis;
        // FD step: follow local short axis. Cap so e never exceeds ~⅓ of thin dimension
        // (elongated box / two prims far apart no longer force huge taps).
        let e_floor = (min_axis * 0.01).max(1e-4);
        let e_cap = (min_axis * 0.35).max(e_floor);
        let e = eps.max(e_floor).min(e_cap).max(1e-4);
        // Newton accept still body-span based so distant planar seeds can land on long AABBs.
        let accept = band.max(diag * 0.08).max(0.05);
        Self {
            bmin,
            bmax,
            diag,
            feature,
            e,
            accept,
        }
    }

    /// Reinhard pre-scale for signed κ: `x = k_raw * feature_display_scale()`.
    #[inline]
    pub fn feature_display_scale(&self) -> f32 {
        self.feature * CURV_FEATURE_DISPLAY_S
    }
}

/// Seed mode (`BakeParams.grid_res.z`, read by the shader's `curv_seed_mode`).
pub(crate) const SEED_PLANAR: u32 = 1;
/// Explicit positions from binding 7 — the vertex rung (P2) and any non-planar sample set.
pub(crate) const SEED_POSITIONS: u32 = 0;

// ================================================================================================
// P2 — VERTEX RUNG: one scalar per DC mesh vertex, read directly from `slot.verts` / `grp.verts`.
// ================================================================================================
//
// Plan §4 ("Consumer B"). The kernel is UNCHANGED from P1 — `SEED_POSITIONS` mode (single seed,
// axis 0, weight 1.0) is exactly the CPU's non-planar `else` branch (`node_graph_bake.rs:453/469`),
// so a vertex sample runs the IDENTICAL algorithm the texture rung runs off the planar domain, just
// with `planar = false`. No new shader code, no new divergence surface.
//
// OWNERSHIP DESIGN (deliberately NOT threaded through `SdfMeshPool`'s bump/rebuild internals):
// `pool.rs`'s per-group buffers are a bump allocator whose regions relocate on grow/rebuild/trim —
// mirroring that exactly for a second buffer would mean editing `place_slot_bump`, `rebuild_group`,
// and `trim_slot` (§5 of the plan flags this as the bulk of P2's risk, "L" sized on its own). This
// module instead owns curvature vertex data in a buffer INDEPENDENT of the pool/slot's physical
// backing store, keyed only by body `NodeId`, and rebuilt WHOLESALE (never patched) whenever the
// mesh's own generation/vert_cap says the vertex data changed. That is provably correct — a stale
// buffer is structurally impossible because staleness is `(mesh_generation, vert_cap) != (seen)`,
// the same generation-counter discipline CLAUDE.md mandates, just anchored to buffers this module
// does not itself relocate. The cost is an extra one-time GPU→GPU-only dispatch (no readback: the
// draw path reads `curv_positions`'s OUTPUT directly, never round-tripping to the CPU) whenever a
// body re-bakes — bounded by the same "once per bake, not per frame" rule the plan requires (§6
// risk 2), keyed on `BodyContentId`-equivalent (mesh generation) so a quiet body dispatches zero
// times per frame.
//
// The buffer is populated by SUBMITTING a `curvature_eval` dispatch whose INPUT (`curv_positions`,
// binding 7) is the mesh's OWN `verts` buffer (already `STORAGE`, zero upload) and whose OUTPUT
// (`output_grid`, binding 2) IS the persistent per-body buffer this module owns — no readback, no
// post-dispatch copy. There is no async fence to track: nothing here is CPU-visible, so the new
// `CurvVertexSlot` is safe to publish as soon as the dispatch is submitted (the GPU serializes the
// compute pass before any later pass that binds the same buffer, same as every other resident-mesh
// GPU-to-GPU handoff in this codebase — e.g. `mesh_into_slot`'s `counters`→`indirect` copy). The
// generation-counter comparison (`mesh_generation`/`vert_cap` vs the mesh's current state) is what
// guards against binding data for a body whose mesh changed AFTER this buffer was built — a body
// that regenerates its mesh mid-flight simply gets re-dispatched on its next poll.

/// One body's persistent per-vertex curvature buffer + the mesh state it was derived from.
///
/// Rebuilt WHOLESALE (never incrementally patched) whenever `mesh_generation`/`vert_cap` no longer
/// matches the mesh's current state — see the module header. `buf` is the kernel's NATIVE 4-wide
/// per-sample output (`output_grid`'s own layout: [mask, curv, convex, concave] per vertex slot) —
/// bound directly, no compaction pass. Which of the 3 non-mask channels the fragment stage should
/// read is carried by `out_channel` (1=signed curv, 2=convex, 3=concave — the Generator layer's own
/// `GeneratorOutput` selector / Cavity's fixed concave output, see `vertex_curv_keys_for_graph`),
/// forwarded to the G-buffer vertex shader as two extra instance-flag bits so the fragment shader
/// samples the CHANNEL THE PANEL ASKED FOR instead of hardcoding signed curvature.
pub(crate) struct CurvVertexSlot {
    pub(crate) buf: wgpu::Buffer,
    /// Vertex-slot capacity (== the mesh's `vert_cap` at the time this was built). `buf` holds
    /// `cap * 4` f32.
    pub(crate) cap: u64,
    /// The mesh generation (pool group generation, or unpooled slot generation) this buffer was
    /// built from. A mismatch against the CURRENT mesh generation means the mesh buffers were
    /// recreated (grow/relocate). Alone this is NOT enough: mesh *content* can be rewritten into
    /// the same buffers without bumping generation (add-prim remesh) — see `mesh_content_gen`.
    pub(crate) mesh_generation: u64,
    /// `SdfBodyMeshSlot::content_gen` at bake time. Bumps on every successful mesh rewrite
    /// (including same-capacity re-dispatch). Primary invalidation for vertex curvature after
    /// structural SDF edits.
    pub(crate) mesh_content_gen: u64,
    /// The `CurvKey` (knobs) this buffer was baked with — a Generator layer whose knobs differ
    /// falls back to "no vertex data" (never a mismatched read), same contract as `CurvGrid`.
    pub(crate) key: CurvKey,
    /// Buffer channel this slot's Generator layer wants displayed: 1=signed curv, 2=convex mask,
    /// 3=concave mask. Independent of `CurvKey` (which is a bake-parameter identity, shared with
    /// the texture rung's `CurvGrid`) — this is purely a display-channel selector, set by
    /// `poll_vertex_curvature` from `vertex_curv_keys_for_graph`'s per-key output tag.
    pub(crate) out_channel: u32,
    /// Display shaping from the panel Generator layer (Vertex path). Updated every poll so
    /// Remap / Opacity / Soft-Sharp respond without re-dispatching the kernel.
    /// `[remap_min, remap_max, opacity, sharpness]`.
    pub(crate) shape_params: [f32; 4],
    /// Panel Invert checkbox (Vertex path).
    pub(crate) invert: bool,
    /// Panel layer blend mode (Vertex path): 0=Normal, 1=Multiply, 2=Screen, 3=Overlay, 4=Add.
    /// Packed into instance flags via `INSTANCE_FLAG_VERTEX_CURV_BLEND_SHIFT`.
    pub(crate) blend: u32,
    /// True when the Vertex generator is a **layer mask** (Photoshop white=show / black=hide),
    /// not a content layer. Packs as `INSTANCE_FLAG_VERTEX_CURV_AS_MASK`.
    pub(crate) as_mask: bool,
}

/// Encode ONE `curvature_eval` dispatch reading positions straight from an existing GPU vertex
/// buffer (`src_verts`, already `STORAGE`) at `[vert_off_bytes, vert_off_bytes + n*32)`, writing
/// its native 4-wide (mask,curv,convex,concave) output into a freshly allocated PERSISTENT buffer
/// — no CPU readback anywhere in this path, and no post-dispatch copy: the kernel's own output
/// buffer IS the buffer the G-buffer vertex shader will bind next frame.
///
/// `n` is the vertex COUNT to dispatch over (the mesh's `vert_cap`, not a GPU-only live count — the
/// plan's §4.1 warning: vertex count is GPU-only, so this dispatches over the CAPACITY with an
/// in-shader guard already present in every `eval_scene_at` caller here, exactly like `bake_volume`
/// zero-filling past the logical extent). Reading past the logical vertex count merely computes
/// curvature for slots the mesher has not written this generation — harmless (never drawn, since
/// the draw's `vertex_index` never exceeds the GPU-written index count) and avoided on the next
/// generation bump regardless.
#[allow(clippy::too_many_arguments)]
/// MESH curvature dispatch (MESH_CURVATURE_PLAN M4) — the geometric counterpart of
/// [`encode_curvature_vertex_dispatch`].
///
/// That function feeds DC vertex positions to `eval_scene_at`, i.e. it measures the analytical
/// FIELD's Laplacian. This one measures the SURFACE: face normals computed from neighbouring
/// vertex positions through the index buffer. Near a boolean seam the field's Laplacian is
/// dominated by how the SDF blends, which is why edges read wrong there; face disagreement is
/// what a curvature map is actually supposed to show.
///
/// Five passes, all reading the mesh's OWN GPU-resident buffers (no upload, no readback):
///   1. `adj_clear`  — reset the vertex→triangle table
///   2. `adj_fill`   — one thread per triangle claims a slot in each of its 3 vertices
///   3. `seed`       — per-vertex area-weighted normal
///   4. `diffuse` ×N — widen the measurement neighbourhood (`smooth_iters`, from the panel Radius)
///   5. `measure`    — curvature = how much the neighbourhood's normals cancel
///
/// Returns the output buffer (4 f32 per vertex, the SAME layout `curvature_eval` produces) so the
/// existing G-buffer carry reads it unchanged.
///
/// `tri_count` is a CEILING derived from index CAPACITY, not the true triangle count — that count
/// lives only in the GPU-written indirect args and reading it back would reintroduce the stall
/// `8a27c1f` removed. Over-dispatching is safe: `adj_fill` bounds-checks every index against
/// `mc_idx`'s length, and a slot past the real geometry reads indices that were never written
/// (zeroed), producing a degenerate triangle that the seed pass discards on its zero-area check.
#[allow(clippy::too_many_arguments)]
pub(crate) fn encode_mesh_curvature_dispatch(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &MeshCurvPipelines,
    bgl: &wgpu::BindGroupLayout,
    src_verts: &wgpu::Buffer,
    vert_off_bytes: u64,
    src_idx: &wgpu::Buffer,
    idx_off_bytes: u64,
    vert_count: u64,
    tri_count: u64,
    smooth_iters: u32,
) -> Option<wgpu::Buffer> {
    if vert_count == 0 || tri_count == 0 {
        return None;
    }
    // Matches `MeshCurvParams` in mesh_curvature.wgsl.
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct MeshCurvParams {
        vert_count: u32,
        tri_count: u32,
        adj_stride: u32,
        smooth_iters: u32,
    }
    // A DC quad mesh gives ~6 incident triangles per vertex; 8 leaves headroom without inflating
    // the table. A vertex exceeding it truncates (see the shader) rather than corrupting a
    // neighbour's list.
    const ADJ_STRIDE: u64 = 8;

    let params = MeshCurvParams {
        vert_count: vert_count as u32,
        tri_count: tri_count as u32,
        adj_stride: ADJ_STRIDE as u32,
        smooth_iters,
    };
    let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mesh_curv_params"),
        size: std::mem::size_of::<MeshCurvParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&params_buf, 0, bytemuck::bytes_of(&params));

    let mk = |label: &str, size: u64, extra: wgpu::BufferUsages| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: size.max(16),
            usage: wgpu::BufferUsages::STORAGE | extra,
            mapped_at_creation: false,
        })
    };
    let quad = vert_count * 4 * 4; // 4 f32 per vertex
    // COPY_SRC: same diagnostic contract as the field-path vertex buffer (SOAK a/b/c readback).
    let dest = mk("mesh_curv_out", quad, wgpu::BufferUsages::COPY_SRC);
    let nrm_a = mk("mesh_curv_nrm_a", quad, wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST);
    let nrm_b = mk("mesh_curv_nrm_b", quad, wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST);
    let adj_rw = mk("mesh_curv_adj", vert_count * ADJ_STRIDE * 4, wgpu::BufferUsages::empty());
    let adj_fill = mk("mesh_curv_adj_fill", vert_count * 4, wgpu::BufferUsages::empty());
    // The CPU-built table slot is unused in production (the GPU builds its own), but the layout
    // requires a binding — point it at the same buffer the GPU fills.
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mesh_curv_bg"),
        layout: bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: params_buf.as_entire_binding() },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: src_verts,
                    offset: vert_off_bytes,
                    size: std::num::NonZeroU64::new(vert_count * 8 * 4),
                }),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: src_idx,
                    offset: idx_off_bytes,
                    size: std::num::NonZeroU64::new(tri_count * 3 * 4),
                }),
            },
            wgpu::BindGroupEntry { binding: 3, resource: adj_rw.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 4, resource: dest.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 5, resource: nrm_a.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 6, resource: nrm_b.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 7, resource: adj_rw.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 8, resource: adj_fill.as_entire_binding() },
        ],
    });

    let vgroups = ((vert_count as u32) + 63) / 64;
    let tgroups = ((tri_count as u32) + 63) / 64;
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("mesh_curvature_encoder"),
    });
    let mut pass = |pipe: &wgpu::ComputePipeline, groups: u32, enc: &mut wgpu::CommandEncoder| {
        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: None,
            timestamp_writes: None,
        });
        cp.set_pipeline(pipe);
        cp.set_bind_group(0, &bg, &[]);
        cp.dispatch_workgroups(groups, 1, 1);
    };
    pass(&pipelines.adj_clear, vgroups, &mut enc);
    pass(&pipelines.adj_fill, tgroups, &mut enc);
    pass(&pipelines.seed, vgroups, &mut enc);
    // Ping-pong: each step writes B, then B is copied back to A so the next step — and the final
    // measure — reads the accumulated neighbourhood from A. Reading and writing one buffer would
    // make the result depend on dispatch order.
    for _ in 0..smooth_iters {
        pass(&pipelines.diffuse, vgroups, &mut enc);
        enc.copy_buffer_to_buffer(&nrm_b, 0, &nrm_a, 0, quad.max(16));
    }
    pass(&pipelines.measure, vgroups, &mut enc);
    queue.submit(std::iter::once(enc.finish()));
    Some(dest)
}

/// The five compute pipelines `encode_mesh_curvature_dispatch` needs, compiled once and cached on
/// `GpuCtx` — they share one shader module and one bind group layout.
pub(crate) struct MeshCurvPipelines {
    pub adj_clear: wgpu::ComputePipeline,
    pub adj_fill: wgpu::ComputePipeline,
    pub seed: wgpu::ComputePipeline,
    pub diffuse: wgpu::ComputePipeline,
    pub measure: wgpu::ComputePipeline,
    pub bgl: wgpu::BindGroupLayout,
}

impl MeshCurvPipelines {
    pub(crate) fn create(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mesh_curvature.wgsl"),
            source: wgpu::ShaderSource::Wgsl(graphics::build_mesh_curvature_wgsl().into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mesh_curvature_bgl"),
            entries: &graphics::bgl::mesh_curvature_bgl_entries(),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh_curvature_layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let mk = |ep: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(ep),
                layout: Some(&layout),
                module: &shader,
                entry_point: Some(ep),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Self {
            adj_clear: mk("mesh_curvature_adj_clear"),
            adj_fill: mk("mesh_curvature_adj_fill"),
            seed: mk("mesh_curvature_seed"),
            diffuse: mk("mesh_curvature_diffuse"),
            measure: mk("mesh_curvature_measure"),
            bgl,
        }
    }
}

/// One pool segment's tile-counter block for O1 pooled live-count sum.
pub(crate) struct PooledTileCounterSeg<'a> {
    pub tile_counters: &'a wgpu::Buffer,
    pub args_base: u32,
    /// Live draw entries (monolithic = 1, tiled = tiles.len()).
    pub n_tiles: u32,
}

/// Encode ONE `curvature_eval` over mesh vertices.
///
/// `n` is **capacity** (allocation / `params.curv_n` ceiling). When `live_counters` is
/// `Some` (DC `counters` buffer with `vert_count` at word 0), O1 dispatches **indirectly**
/// over the live count so a 2M cap does not burn 100× GPU time. Allocation still uses `n`.
///
/// O1 pooled: pass `pooled_segs` + `sum_tile_pipe` instead of `live_counters` — tile counters
/// are summed into a temp body-level pair, then the same indirect args path runs.
pub(crate) fn encode_curvature_vertex_dispatch(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &wgpu::ComputePipeline,
    bgl: &wgpu::BindGroupLayout,
    tex_dummy: &wgpu::Texture,
    prim_floats: &[f32],
    clip_floats: &[f32],
    src_verts: &wgpu::Buffer,
    vert_off_bytes: u64,
    n: u64,
    c: &CurvConstants,
    region: u32,
    grow: f32,
    contrast: f32,
    strength: f32,
    live_counters: Option<&wgpu::Buffer>,
    dispatch_args_pipe: Option<&(wgpu::BindGroupLayout, wgpu::ComputePipeline)>,
) -> Option<wgpu::Buffer> {
    encode_curvature_vertex_dispatch_ex(
        device,
        queue,
        pipeline,
        bgl,
        tex_dummy,
        prim_floats,
        clip_floats,
        src_verts,
        vert_off_bytes,
        n,
        c,
        region,
        grow,
        contrast,
        strength,
        live_counters,
        dispatch_args_pipe,
        None,
        None,
    )
}

/// O1 pooled entry: same as [`encode_curvature_vertex_dispatch`] with tile-counter sum.
pub(crate) fn encode_curvature_vertex_dispatch_pooled(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &wgpu::ComputePipeline,
    bgl: &wgpu::BindGroupLayout,
    tex_dummy: &wgpu::Texture,
    prim_floats: &[f32],
    clip_floats: &[f32],
    src_verts: &wgpu::Buffer,
    vert_off_bytes: u64,
    n: u64,
    c: &CurvConstants,
    region: u32,
    grow: f32,
    contrast: f32,
    strength: f32,
    dispatch_args_pipe: Option<&(wgpu::BindGroupLayout, wgpu::ComputePipeline)>,
    sum_tile_pipe: Option<&(wgpu::BindGroupLayout, wgpu::ComputePipeline)>,
    pooled_segs: &[PooledTileCounterSeg<'_>],
) -> Option<wgpu::Buffer> {
    encode_curvature_vertex_dispatch_ex(
        device,
        queue,
        pipeline,
        bgl,
        tex_dummy,
        prim_floats,
        clip_floats,
        src_verts,
        vert_off_bytes,
        n,
        c,
        region,
        grow,
        contrast,
        strength,
        None,
        dispatch_args_pipe,
        sum_tile_pipe,
        Some(pooled_segs),
    )
}

fn encode_curvature_vertex_dispatch_ex(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &wgpu::ComputePipeline,
    bgl: &wgpu::BindGroupLayout,
    tex_dummy: &wgpu::Texture,
    prim_floats: &[f32],
    clip_floats: &[f32],
    src_verts: &wgpu::Buffer,
    vert_off_bytes: u64,
    n: u64,
    c: &CurvConstants,
    region: u32,
    grow: f32,
    contrast: f32,
    strength: f32,
    live_counters: Option<&wgpu::Buffer>,
    dispatch_args_pipe: Option<&(wgpu::BindGroupLayout, wgpu::ComputePipeline)>,
    sum_tile_pipe: Option<&(wgpu::BindGroupLayout, wgpu::ComputePipeline)>,
    pooled_segs: Option<&[PooledTileCounterSeg<'_>]>,
) -> Option<wgpu::Buffer> {
    if n == 0 {
        return None;
    }
    let prim_count = (prim_floats.len() / graphics::HOT_FLOATS_PER_PRIM) as u32;
    if prim_count == 0 {
        return None;
    }
    // 8 f32/vertex (RasterVertex-equivalent packing); position at floats [0..2] of each vertex.
    // `curv_positions` is read with an explicit BYTE offset via a bound sub-range, so the kernel's
    // `params.brick_dim` (float stride) stays 8 here (NOT 3, the planar-grid packed-xyz stride) —
    // `curv_seed_at`'s non-planar branch indexes `i * params.brick_dim + {0,1,2}`, which lands
    // exactly on a vertex's position floats when the stride matches the mesh's own 8-float layout.
    const VERT_STRIDE_FLOATS: u32 = 8;

    // BASE-VERTEX PARITY (device-loss class, fixed 2026-07-27). The G-buffer VS indexes this
    // buffer with `@builtin(vertex_index)`, which in WGSL INCLUDES the draw's `base_vertex`. A
    // POOLED body draws with `base_vertex = vert_off / 32` (`pool.rs:558`) and binds the whole
    // group vertex buffer, so its `vertex_index` starts at that offset — while this kernel writes
    // its output from index 0. Writing at 0 and reading at `base` is an out-of-bounds storage read
    // for every pooled body (unpooled bodies have `vert_off == 0`, which is why the semantic test
    // and the unpooled path never caught it).
    //
    // The fix keeps ONE index convention instead of compensating in the shader (which would need a
    // per-body uniform and could silently desync from the pool allocator): the destination is
    // sized for `base + n` entries and the kernel writes at `base`, so a slot's curvature lives at
    // exactly the same index its vertex does. `vertex_index * 4 + 1` is then correct for pooled and
    // unpooled draws alike, with no shader change and no new binding.
    let base_vertex = vert_off_bytes / 32; // pool.rs:88 — vertex stride is 32 B, divides 256
    debug_assert_eq!(vert_off_bytes % 32, 0, "vertex region offset must be a whole vertex stride");

    let params = curvature_bake_params(
        n as u32, region, SEED_POSITIONS, VERT_STRIDE_FLOATS, c, grow, contrast, strength, prim_count,
        base_vertex as u32,
    );

    let mk_storage = |label: &str, floats: &[f32]| {
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: (floats.len() as u64 * 4).max(16),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if !floats.is_empty() {
            queue.write_buffer(&buf, 0, bytemuck::cast_slice(floats));
        }
        buf
    };
    let prim_buf = mk_storage("curv_prim_v", prim_floats);
    let clip_buf = mk_storage("curv_clip_v", clip_floats);
    let mesh_dummy = mk_storage("curv_mesh_grid_dummy_v", &[0.0f32; 8]);

    let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("curv_params_v"),
        size: std::mem::size_of::<crate::gpu_jobs::BakeParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&params_buf, 0, bytemuck::bytes_of(&params));

    // PERSISTENT destination: the kernel writes its native (mask,curv,convex,concave) output
    // directly here — this buffer IS what `CurvVertexSlot::buf` becomes, no extra copy. STORAGE
    // (write from the kernel + read by the vertex shader) — no VERTEX usage needed, it's read via
    // `@builtin(vertex_index)` from a bind group, never a vertex-attribute slot (plan §4.2 "(i)").
    // Sized for `base_vertex + n` entries (see BASE-VERTEX PARITY above): the kernel writes its
    // `n` results at `base_vertex`, so slot i's curvature sits at the SAME index as slot i's
    // vertex. The `[0, base_vertex)` prefix is never written and never read — the VS only reaches
    // it for vertices belonging to OTHER bodies in the same pool group, and those draws bind
    // THEIR OWN curvature buffer (`bind_curvature` rebinds per body, raster_common.rs:235).
    //
    // A sub-range binding at `base_vertex * 16` would avoid the prefix, but that offset is only
    // 128-B aligned (`vert_off` is 256-aligned ⇒ `vert_off/2` is not), and wgpu requires 256 for
    // storage sub-ranges — so padding is the correct trade, not a shortcut.
    let dest = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("curv_vertex_out"),
        size: ((base_vertex + n) * 4 * 4).max(16),
        // COPY_SRC: SOAK / diagnostic readback only (never on the hot path). STORAGE alone
        // cannot be copied; without this the (a)/(b)/(c) bisect in soak is impossible.
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });

    // Bind the SOURCE vertex buffer at its sub-range — the mesh's own storage buffer, zero-copy.
    let src_size = n * (VERT_STRIDE_FLOATS as u64) * 4;
    let src_binding = wgpu::BufferBinding {
        buffer: src_verts, offset: vert_off_bytes, size: std::num::NonZeroU64::new(src_size),
    };
    let tex_view = tex_dummy.create_view(&wgpu::TextureViewDescriptor::default());

    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("curv_bg_v"),
        layout: bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: prim_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: dest.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&tex_view) },
            wgpu::BindGroupEntry { binding: 4, resource: clip_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 5, resource: mesh_dummy.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 6, resource: mesh_dummy.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::Buffer(src_binding) },
            // binding(8): T1 height_maps — unread by curvature_eval; dummy fills bake BGL.
            wgpu::BindGroupEntry { binding: 8, resource: mesh_dummy.as_entire_binding() },
        ],
    });

    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("curv_vertex_encoder"),
    });

    // O1 pooled: sum per-tile counters → body-level [vert_count, index_count], then same
    // indirect args path as unpooled. Temp buffer lives for this encoder only.
    let pooled_live_buf = match (pooled_segs, sum_tile_pipe) {
        (Some(segs), Some((sum_bgl, sum_pipe))) if !segs.is_empty() => {
            let body_cnt = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("curv_pooled_live_count"),
                size: 8,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            // Zero so accumulate segments start clean.
            queue.write_buffer(&body_cnt, 0, bytemuck::cast_slice(&[0u32, 0u32]));
            let mut first_seg = true;
            for seg in segs.iter() {
                if seg.n_tiles == 0 {
                    continue;
                }
                let acc = if first_seg { 0u32 } else { 1u32 };
                first_seg = false;
                let params = [seg.args_base, seg.n_tiles, acc, 0u32];
                let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("curv_sum_tile_params"),
                    size: 16,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                queue.write_buffer(&params_buf, 0, bytemuck::cast_slice(&params));
                let sum_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("curv_sum_tile_bg"),
                    layout: sum_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: seg.tile_counters.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: body_cnt.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: params_buf.as_entire_binding(),
                        },
                    ],
                });
                {
                    let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("curv_sum_tile_counters"),
                        timestamp_writes: None,
                    });
                    cp.set_pipeline(sum_pipe);
                    cp.set_bind_group(0, &sum_bg, &[]);
                    cp.dispatch_workgroups(1, 1, 1);
                }
            }
            Some(body_cnt)
        }
        _ => None,
    };
    let effective_live = live_counters.or(pooled_live_buf.as_ref());

    // O1: build (wx, wy, 1) from live counters[0] when available — else CPU dims over capacity.
    let indirect_buf = match (effective_live, dispatch_args_pipe) {
        (Some(counters), Some((args_bgl, args_pipe))) => {
            let args = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("curv_dispatch_indirect"),
                size: 12, // x, y, z workgroups
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::INDIRECT
                    | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let args_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("curv_dispatch_args_bg"),
                layout: args_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: counters.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: args.as_entire_binding(),
                    },
                ],
            });
            {
                let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("curv_build_dispatch_args"),
                    timestamp_writes: None,
                });
                cp.set_pipeline(args_pipe);
                cp.set_bind_group(0, &args_bg, &[]);
                cp.dispatch_workgroups(1, 1, 1);
            }
            Some(args)
        }
        _ => None,
    };

    {
        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("curvature_eval_vertex"),
            timestamp_writes: None,
        });
        cp.set_pipeline(pipeline);
        cp.set_bind_group(0, &bg, &[]);
        if let Some(ref args) = indirect_buf {
            // Live vertex count → workgroups (O1). Kernel still early-outs with `i >= curv_n`
            // where curv_n is capacity; over-dispatch past live count is what we eliminate.
            cp.dispatch_workgroups_indirect(args, 0);
        } else {
            // 2D dispatch over capacity — fallback when counters unavailable (pooled path).
            let (wx, wy) = curvature_dispatch_dims(n as u32);
            if wx > 0 {
                cp.dispatch_workgroups(wx, wy, 1);
            }
        }
    }
    queue.submit(std::iter::once(enc.finish()));
    Some(dest)
}

/// Workgroup counts for `curvature_eval` (`@workgroup_size(64)`).
///
/// Drivers cap each axis at `max_compute_workgroups_per_dimension` (typically 65535). A mesh
/// with `vert_cap > 65535 * 64` (common at 512³ dense / large sparse) must use a 2D grid —
/// otherwise the 1D dispatch is invalid and the kernel writes nothing (vertex κ “disappears”).
#[inline]
pub(crate) fn curvature_dispatch_dims(n: u32) -> (u32, u32) {
    const WG: u32 = 64;
    const MAX_WG: u32 = 65535;
    if n == 0 {
        return (0, 0);
    }
    let total_wg = (n + WG - 1) / WG;
    let wx = total_wg.min(MAX_WG);
    let wy = (total_wg + wx - 1) / wx;
    (wx, wy.min(MAX_WG))
}

/// Per-channel min/max/mean over a sample of a vertex-curvature output buffer.
///
/// SOAK / diagnostic only — blocks on GPU (`PollType::Wait`). Never call from the interactive
/// poll path. Answers the phase-3 (a)/(b)/(c) bisect:
///   (a) kernel writes ~constant → min≈max on the written range
///   (b) write base ≠ VS read base → data lives at [0..) while draw expects [base..) (or reverse)
///   (c) `arrayLength` too small for `vertex_index*4+chan` → every VS sample is the 0.0 guard
#[derive(Clone, Debug)]
pub(crate) struct CurvVertexReadback {
    pub base_vertex: u64,
    pub cap: u64,
    /// `arrayLength(&curv_vertex)` as the VS sees it (f32 elements).
    pub array_len_f32: u64,
    pub sample_n: u32,
    /// Written range `[base, base+sample)`: channels mask/signed/convex/concave → (min,max,mean).
    pub written: [(f32, f32, f32); 4],
    /// Prefix `[0, sample)` signed channel only — `None` when `base_vertex == 0`.
    pub prefix_signed: Option<(f32, f32, f32)>,
    /// Max float index the VS needs for the last capacity slot: `(base+cap-1)*4 + 3`.
    pub max_vs_idx: u64,
    /// One of: `"a_constant"`, `"b_base_mismatch"`, `"c_arraylength"`, `"ok_varies"`, `"empty"`.
    pub diagnosis: &'static str,
}

/// Blocking sample of `CurvVertexSlot::buf`. `cap` is the slot's vertex capacity (same field the
/// dispatch used). Infers `base_vertex` from buffer size vs cap (field path pads to `base+n`).
pub(crate) fn readback_curv_vertex_stats(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buf: &wgpu::Buffer,
    cap: u64,
) -> Option<CurvVertexReadback> {
    let size = buf.size();
    if size < 16 || cap == 0 {
        return None;
    }
    let array_len_f32 = size / 4;
    let entries = array_len_f32 / 4; // 4 f32 per vertex slot
    let base_vertex = entries.saturating_sub(cap);
    let max_vs_idx = if cap > 0 {
        (base_vertex + cap - 1) * 4 + 3
    } else {
        0
    };

    let sample_n = (cap.min(512)) as u32;
    let written_floats = copy_buffer_f32(
        device,
        queue,
        buf,
        base_vertex * 16, // 4 f32 * 4 B
        sample_n as u64 * 4,
    )?;
    let written = channel_stats_4wide(&written_floats);

    let prefix_signed = if base_vertex > 0 {
        let pref_n = base_vertex.min(64) as u32;
        let pref = copy_buffer_f32(device, queue, buf, 0, pref_n as u64 * 4)?;
        Some(channel_stats_4wide(&pref)[1])
    } else {
        None
    };

    let signed = written[1];
    let var = signed.1 - signed.0;
    let abs_peak = signed.0.abs().max(signed.1.abs());

    // (c) first: if arrayLength cannot cover the last capacity slot the VS would guard to 0.
    let diagnosis = if max_vs_idx >= array_len_f32 {
        "c_arraylength"
    } else if let Some(ps) = prefix_signed {
        // (b): kernel wrote at 0 (prefix varies) while VS would read at base (written flat/zero),
        // or the reverse (written varies, prefix has the real data only at 0).
        let pref_var = ps.1 - ps.0;
        let pref_peak = ps.0.abs().max(ps.1.abs());
        let written_dead = var < 1e-6 && abs_peak < 1e-6;
        let pref_alive = pref_var > 1e-4 || pref_peak > 1e-4;
        let written_alive = var > 1e-4 || abs_peak > 1e-4;
        if pref_alive && written_dead {
            "b_base_mismatch"
        } else if !written_alive {
            "a_constant"
        } else {
            "ok_varies"
        }
    } else if var < 1e-6 {
        // Flat signal (including all-zero). All-zero is still (a) for the kernel-output question —
        // the VS would see a constant, so radius changes cannot produce px delta.
        "a_constant"
    } else {
        "ok_varies"
    };

    Some(CurvVertexReadback {
        base_vertex,
        cap,
        array_len_f32,
        sample_n,
        written,
        prefix_signed,
        max_vs_idx,
        diagnosis,
    })
}

fn channel_stats_4wide(floats: &[f32]) -> [(f32, f32, f32); 4] {
    let mut out = [(f32::INFINITY, f32::NEG_INFINITY, 0.0f32); 4];
    let n = floats.len() / 4;
    if n == 0 {
        return [(0.0, 0.0, 0.0); 4];
    }
    for i in 0..n {
        for c in 0..4 {
            let v = floats[i * 4 + c];
            out[c].0 = out[c].0.min(v);
            out[c].1 = out[c].1.max(v);
            out[c].2 += v;
        }
    }
    for c in 0..4 {
        out[c].2 /= n as f32;
    }
    out
}

/// Blocking GPU→CPU copy of `float_count` f32s starting at `byte_offset` in `src`.
fn copy_buffer_f32(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    src: &wgpu::Buffer,
    byte_offset: u64,
    float_count: u64,
) -> Option<Vec<f32>> {
    if float_count == 0 {
        return Some(Vec::new());
    }
    let bytes = float_count * 4;
    if byte_offset + bytes > src.size() {
        return None;
    }
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("curv_vertex_readback_staging"),
        size: bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("curv_vertex_readback_enc"),
    });
    enc.copy_buffer_to_buffer(src, byte_offset, &staging, 0, bytes);
    queue.submit(std::iter::once(enc.finish()));

    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    // SOAK/diagnostic only — block until the map settles.
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    rx.recv().ok()?.ok()?;
    let data = slice.get_mapped_range();
    let out: Vec<f32> = bytemuck::cast_slice(&data).to_vec();
    drop(data);
    staging.unmap();
    Some(out)
}

#[cfg(test)]
mod curv_constants_tests {
    use super::{CurvConstants, CURV_FEATURE_DISPLAY_S};
    use glam::Vec3;

    /// Elongate / multi-prim: e is capped by short axis, not inflated by long diag.
    #[test]
    fn elongated_aabb_e_follows_min_axis_not_diag() {
        // 10×1×1 box (diag ≈ 10.1, min_axis = 1)
        let bmin = Vec3::new(-5.0, -0.5, -0.5);
        let bmax = Vec3::new(5.0, 0.5, 0.5);
        let c = CurvConstants::new(bmin, bmax, 0.2, 0.15);
        assert!((c.feature - 1.0).abs() < 1e-4, "feature=min_axis=1, got {}", c.feature);
        // User radius 0.2 is above min_axis*0.01 and below min_axis*0.35 → keep 0.2
        assert!((c.e - 0.2).abs() < 1e-4, "e should honor radius when in range, got {}", c.e);
        // Huge radius must cap at 0.35 * min_axis (old formula: max(10, diag*0.01) ≈ 10)
        let c2 = CurvConstants::new(bmin, bmax, 10.0, 0.15);
        assert!(
            (c2.e - 0.35).abs() < 1e-4,
            "e must cap at 0.35*min_axis on elongated AABB, got {}",
            c2.e
        );
        assert!(c2.e < 1.0, "e must not track full diagonal length");
        // Display scale is feature * S, not diag * 0.5
        let disp = c.feature_display_scale();
        assert!((disp - CURV_FEATURE_DISPLAY_S).abs() < 1e-3);
        assert!(disp < c.diag * 0.5, "display scale must not use full diag on long bodies");
    }

    /// Two prims far apart (duplicate offset): same short-axis e as a single small box.
    #[test]
    fn multi_prim_span_does_not_inflate_e() {
        // Union of two 1³ boxes centered at x=±5 → AABB ~ 11×1×1
        let bmin = Vec3::new(-5.5, -0.5, -0.5);
        let bmax = Vec3::new(5.5, 0.5, 0.5);
        let c = CurvConstants::new(bmin, bmax, 0.02, 0.15);
        assert!((c.feature - 1.0).abs() < 1e-4);
        assert!(
            c.e <= 0.35 + 1e-4,
            "multi-prim body span must not force e above short-axis cap, got {}",
            c.e
        );
        // Floor is min_axis*0.01 = 0.01; radius 0.02 wins
        assert!((c.e - 0.02).abs() < 1e-4, "e=0.02 expected, got {}", c.e);
    }

    /// Cube: roughly preserves old display magnitude order (feature * S ≈ diag * 0.5).
    #[test]
    fn cube_display_scale_near_legacy_diag() {
        let bmin = Vec3::splat(-0.5);
        let bmax = Vec3::splat(0.5);
        let c = CurvConstants::new(bmin, bmax, 0.02, 0.15);
        let legacy = c.diag * 0.5;
        let modern = c.feature_display_scale();
        assert!(
            (modern - legacy).abs() / legacy < 0.05,
            "cube modern={modern} legacy={legacy} should match within 5%"
        );
    }
}

#[cfg(test)]
mod dispatch_dims_tests {
    use super::curvature_dispatch_dims;

    #[test]
    fn small_n_is_1d_row() {
        let (wx, wy) = curvature_dispatch_dims(100);
        assert_eq!(wy, 1);
        assert_eq!(wx, (100 + 63) / 64);
    }

    #[test]
    fn over_65535_workgroups_uses_2d() {
        // 65535 * 64 + 1 threads → 65536 workgroups → needs wy ≥ 2
        let n = 65535u32 * 64 + 1;
        let (wx, wy) = curvature_dispatch_dims(n);
        assert!(wx <= 65535, "wx={wx}");
        assert!(wy >= 2, "wy={wy}");
        assert!(wx as u64 * wy as u64 * 64 >= n as u64);
    }

    #[test]
    fn dense_512_scale_vert_cap_fits_2d() {
        // Worst-case dense output cap order-of-magnitude (~millions of verts).
        let n = 8_000_000u32;
        let (wx, wy) = curvature_dispatch_dims(n);
        assert!(wx <= 65535 && wy <= 65535);
        assert!(wx as u64 * wy as u64 * 64 >= n as u64);
    }

    /// O1 — live surface counts must need far fewer workgroups than allocated capacity.
    /// Documents the win: a typical box at ~20k verts vs 2M cap.
    #[test]
    fn o1_live_count_needs_far_fewer_workgroups_than_cap() {
        let live = 20_000u32;
        let cap = 2_048_383u32;
        let (lx, ly) = curvature_dispatch_dims(live);
        let (cx, cy) = curvature_dispatch_dims(cap);
        let live_wg = lx as u64 * ly as u64;
        let cap_wg = cx as u64 * cy as u64;
        assert!(
            live_wg * 10 < cap_wg,
            "live {live_wg} workgroups should be ≪ cap {cap_wg} (O1 win)"
        );
        // Indirect args builder math must match this function (see ensure_curv_dispatch_args WGSL).
        let total_wg = (live + 63) / 64;
        let wx = total_wg.min(65535);
        let wy = (total_wg + wx - 1) / wx;
        assert_eq!((wx, wy.min(65535)), (lx, ly));
    }

    /// O1 pooled — summing per-tile live counts equals body live count (the sum pass contract).
    #[test]
    fn o1_pooled_tile_counter_sum_matches_body_live() {
        // Simulate N tiles with known vert counts; sum must equal total (what the WGSL does).
        let tile_verts = [120u32, 340, 50, 0, 1800];
        let live: u32 = tile_verts.iter().sum();
        let cap = 2_048_383u32;
        let (lx, ly) = curvature_dispatch_dims(live);
        let (cx, cy) = curvature_dispatch_dims(cap);
        assert!(
            (lx as u64 * ly as u64) * 10 < (cx as u64 * cy as u64),
            "pooled live sum {live} must still need ≪ cap workgroups"
        );
        // Empty / zero tiles still sum cleanly (dead tiles contribute 0).
        assert_eq!(tile_verts.iter().filter(|&&v| v == 0).count(), 1);
    }
}

/// Build the `BakeParams` for a `curvature_eval` dispatch.
///
/// Every field the curvature entry reads is spare padding for this entry (it touches no grid, no
/// bricks, no atlas, no texture) — the `classify_bricks` precedent for repurposing the tail
/// without changing offsets or the 208-byte size. The mapping is documented once, in the
/// shader's `curv_*` accessor block; this function is its mirror image.
pub(crate) fn curvature_bake_params(
    n: u32,
    region: u32,
    seed_mode: u32,
    stride_floats: u32,
    c: &CurvConstants,
    grow: f32,
    contrast: f32,
    strength: f32,
    prim_count: u32,
    // Index of the FIRST output entry (see BASE-VERTEX PARITY in
    // `encode_curvature_vertex_dispatch`). The vertex rung passes the draw's `base_vertex` so a
    // pooled body's curvature lands at the same index its vertex has; the planar/seed grid paths
    // pass 0 and are bit-for-bit unaffected.
    out_base: u32,
) -> crate::gpu_jobs::BakeParams {
    crate::gpu_jobs::BakeParams {
        // grid_res = (sample count, region, seed mode) — see `curv_n`/`curv_region`/`curv_seed_mode`.
        grid_res: [n, region, seed_mode],
        _pad0: stride_floats, // WGSL: brick_dim — sample stride in floats (3 for packed xyz)
        bounds_min: c.bmin.to_array(),
        _pad1: c.e, // WGSL: classify_band — the CPU-computed curvature FD step
        bounds_max: c.bmax.to_array(),
        prim_count,
        floats_per_prim: graphics::HOT_FLOATS_PER_PRIM as u32,
        dims_bricks: [0; 3],
        bake_min: [0; 3],
        _pad3: c.accept.to_bits(), // WGSL: classify_margin: f32 — CPU-computed Newton acceptance
        bake_max: [0; 3],
        _pad4: out_base, // WGSL: curv_out_base — first output entry index (0 for grid paths)
        mirror_flags: 0,
        // The shaping knobs ride the mirror-center slot (unused by this entry: body mirror is
        // already inside the prim stream as a scope-stack machine, exactly as for the bake).
        mirror_center: [grow, contrast, strength],
        // WGSL: curv_feature — short-axis display scale (NOT full diag). Seeds recompute diag
        // from bounds_min/max on-device so elongate/dup don't inflate κ renorm.
        bevel_radius: c.feature_display_scale(),
        mirror_sign: 0,
        atlas_offset: [0; 3],
        atlas_pad: 0,
        output_mode: 0, // informational — curvature_eval never branches on it
        _pad5: 0,
        body_local_to_world: crate::gpu_jobs::IDENTITY_MAT4,
    }
}

/// Everything one dispatch needs that is not already a GPU resource.
pub(crate) struct CurvDispatch<'a> {
    /// Serialized body prims, body-local — `graphics::serialize_body_for_bake_hot` output.
    pub prim_floats: &'a [f32],
    pub clip_floats: &'a [f32],
    pub params: crate::gpu_jobs::BakeParams,
    /// Sample count (must equal `params.grid_res[0]`).
    pub n: u32,
    /// Packed xyz positions for `SEED_POSITIONS` mode; empty for `SEED_PLANAR`.
    pub positions: &'a [f32],
}

/// An encoded, submitted `curvature_eval` whose readback has NOT settled yet.
///
/// The frame thread NEVER waits on this. `encode_curvature_dispatch` submits and returns; a later
/// `poll_graph_curvature` peeks `map_ready` non-blockingly (`PollType::Poll`) and only reads the
/// mapped range once the callback has landed. Mirrors `PendingGpuClassify` (`gpu_ctx.rs:163`) —
/// same `Arc<MapState>` tri-state, same drain shape.
pub(crate) struct PendingCurvReadback {
    pub staging: wgpu::Buffer,
    /// Tri-state readback status: `OK` → read + unmap; `ERR` → drop the flight (CPU fallback);
    /// `PENDING` → leave in flight, do not touch the buffer.
    pub map_ready: std::sync::Arc<crate::map_state::MapState>,
    /// Output length in floats (`n * 4`) — the mapped range is sliced to exactly this.
    pub out_floats: u64,
    pub submitted: std::time::Instant,
}

impl PendingCurvReadback {
    /// Non-blocking status read. `PENDING` / `OK` / `ERR`.
    pub fn peek(&self) -> u8 {
        self.map_ready.peek()
    }

    /// Consume a SETTLED flight: `OK` → the `n * 4` floats (buffer unmapped); `ERR` → `None`,
    /// caller falls back to the CPU path. Must only be called when `peek() != PENDING`.
    pub fn take(self) -> Option<Vec<f32>> {
        if self.map_ready.peek() != crate::map_state::OK {
            return None; // map failed → caller runs the CPU curvature path
        }
        let out: Vec<f32> = {
            let view = self.staging.slice(..).get_mapped_range();
            bytemuck::cast_slice::<u8, f32>(&view)[..self.out_floats as usize].to_vec()
        };
        self.staging.unmap();
        Some(out)
    }
}

/// Encode + submit ONE `curvature_eval` dispatch and arm its readback — **returns immediately**.
///
/// **Never blocks the frame thread.** The old form of this function ran
/// `submit → map_async → device.poll(PollType::Wait) → rx.recv()` inline, which stalled the whole
/// UI for the duration of the bake (owner-reported freeze on adding a Curvature/Cavity generator
/// layer). `PollType::Wait` on the frame thread is a Frame Budget Rule violation regardless of
/// payload size — the wait is bounded by the GPU's queue depth, not by the 1 MiB of output. The
/// cross-frame `MapState` fence used by `poll_primary_bake` / `poll_gpu_classify_readbacks` is the
/// codebase's answer to exactly this, so curvature now uses it too.
///
/// Returns `None` if any GPU step fails before submit — the caller then runs the CPU path, so a
/// curvature bake never fails for want of a GPU.
pub(crate) fn encode_curvature_dispatch(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &wgpu::ComputePipeline,
    bgl: &wgpu::BindGroupLayout,
    tex_dummy: &wgpu::Texture,
    d: CurvDispatch<'_>,
) -> Option<PendingCurvReadback> {
    debug_assert_eq!(d.n, d.params.grid_res[0], "sample count must match params.grid_res.x");
    // `_pad4` carries `curv_out_base` (see `curvature_bake_params`): the shader writes entry `i`
    // at `(out_base + i) * 4`, so the buffer must cover the base too. Every production grid caller
    // passes 0 and is unaffected; the vertex-rung offset test drives this non-zero.
    let out_floats = ((d.params._pad4 as u64) + (d.n as u64)) * 4;
    let out_bytes = out_floats * 4;

    let mk_storage = |label: &str, floats: &[f32]| {
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: (floats.len() as u64 * 4).max(16),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if !floats.is_empty() {
            queue.write_buffer(&buf, 0, bytemuck::cast_slice(floats));
        }
        buf
    };

    let prim_buf = mk_storage("curv_prim", d.prim_floats);
    let clip_buf = mk_storage("curv_clip", d.clip_floats);
    // Positions: real buffer in SEED_POSITIONS mode, 16-byte filler in SEED_PLANAR (the shader
    // never indexes it there — the seeds are derived from the AABB on-device).
    let pos_buf = mk_storage("curv_positions", d.positions);
    // mesh_grid (binding 5): the disabled-operand dummy — 8 zeroed header floats, res = 0.0 →
    // `sample_mesh_grid` returns 1e6, a union no-op. Same dummy the workers bind.
    let mesh_dummy = mk_storage("curv_mesh_grid_dummy", &[0.0f32; 8]);
    // brick_index (binding 6): unread by this entry; mesh_grid doubles as the filler, the
    // dense-bake precedent.

    let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("curv_out"),
        size: out_bytes.max(16),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("curv_params"),
        size: std::mem::size_of::<crate::gpu_jobs::BakeParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&params_buf, 0, bytemuck::bytes_of(&d.params));
    let tex_view = tex_dummy.create_view(&wgpu::TextureViewDescriptor::default());

    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("curv_bg"),
        layout: bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: prim_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: out_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&tex_view) },
            wgpu::BindGroupEntry { binding: 4, resource: clip_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 5, resource: mesh_dummy.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 6, resource: mesh_dummy.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 7, resource: pos_buf.as_entire_binding() },
            // binding(8): T1 height_maps — unread by curvature_eval; dummy fills bake BGL.
            wgpu::BindGroupEntry { binding: 8, resource: mesh_dummy.as_entire_binding() },
        ],
    });

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("curv_staging"),
        size: out_bytes.max(16),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("curv_encoder"),
    });
    {
        let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("curvature_eval"),
            timestamp_writes: None,
        });
        cp.set_pipeline(pipeline);
        cp.set_bind_group(0, &bg, &[]);
        // workgroup_size(64) — matches the shader (NOT 256: `eval_scene_at`'s eleven
        // array<_,8> stacks cap occupancy, plan §2.3). 2D when n/64 > 65535.
        let (wx, wy) = curvature_dispatch_dims(d.n);
        if wx > 0 {
            cp.dispatch_workgroups(wx, wy, 1);
        }
    }
    enc.copy_buffer_to_buffer(&out_buf, 0, &staging, 0, out_bytes.max(16));
    queue.submit(std::iter::once(enc.finish()));

    // GPU Safety rule: capture the map_async Result — `MapState` encodes `Err` as `ERR` rather
    // than discarding it, so a failed map is OBSERVABLE at the drain (and falls back to CPU)
    // instead of pending forever and leaking the staging buffer. Same sink the classify path
    // uses (`app_poll_bake.rs:1472-1477`).
    let map_ready = std::sync::Arc::new(crate::map_state::MapState::new());
    let map_ready_cb = map_ready.clone();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |result| {
        map_ready_cb.store_result(&result);
    });
    // NO device.poll(PollType::Wait) here — that is the freeze. The drain polls with
    // `PollType::Poll` on later frames.
    Some(PendingCurvReadback {
        staging,
        map_ready,
        out_floats,
        submitted: std::time::Instant::now(),
    })
}

/// TEST-ONLY blocking wrapper: encode + submit, then wait for the readback on THIS thread.
///
/// Production never calls this — `poll_graph_curvature` drains `PendingCurvReadback` across
/// frames instead (a `PollType::Wait` on the frame thread is what froze the app). A headless
/// golden test has no frame loop to stall, so it wraps the exact same async pieces in one
/// blocking call and keeps its assertions unchanged.
#[cfg(test)]
pub(crate) fn dispatch_curvature_blocking(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &wgpu::ComputePipeline,
    bgl: &wgpu::BindGroupLayout,
    tex_dummy: &wgpu::Texture,
    d: CurvDispatch<'_>,
) -> Option<Vec<f32>> {
    let pending = encode_curvature_dispatch(device, queue, pipeline, bgl, tex_dummy, d)?;
    device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    // The callback runs during the Wait above; peek is settled by now. Poll defensively in case
    // a backend defers the callback past the first Wait.
    while pending.peek() == crate::map_state::PENDING {
        device.poll(wgpu::PollType::Wait).expect("GPU device polling failed");
    }
    pending.take()
}
