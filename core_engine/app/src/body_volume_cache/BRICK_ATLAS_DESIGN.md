# Brick Pool + Atlas — Composable Design

## Key Insight

Atlas and brick pool are NOT alternatives. They compose:

```
Layer 1: Atlas texture (GPU rendering contract)
  - Single 3D texture, VP shader samples from it
  - Per-body Z-slabs with UVW mapping
  - Unchanged by brick pool — stable rendering API

Layer 2: Brick pool (sparse storage, fills the atlas)
  - 8³ bricks allocated only near the surface
  - Indirection map: brick_coord → pool_slot
  - Bake dispatches only for allocated bricks
  - Writes results into the atlas texture via textureStore
```

## What changes with bricks

| Component | Without bricks | With bricks |
|-----------|---------------|-------------|
| Atlas texture | Same | Same |
| VP shader sample_sdf() | Same | Same |
| UVW contract | Same | Same |
| Bake dispatch | All res³ voxels | Only allocated bricks (~5% of voxels) |
| Atlas Z-slab content | Dense grid | Sparse — empty regions stay at +999.0 |
| Memory per body | res³ × 4 bytes | ~5% of that (surface bricks only) |
| Effective resolution | res³ | Up to 8x higher (same memory, fewer wasted voxels) |

## Why the 23 bugs happened

The parked brick pool tried to be a SEPARATE rendering path with its own
indirection texture sampled in the VP shader. This created:
- Two code paths in the VP shader (dense vs brick)
- Cache coherency issues (indirection texture misses)
- Complex lifecycle (brick alloc/free/defrag)

The correct approach: bricks write INTO the atlas. One rendering path.
The VP shader never knows about bricks — it just reads the atlas.

## Implementation path

1. Keep atlas as-is (rendering contract, stable)
2. Add brick allocator: tracks which 8³ regions contain surface
3. Bake dispatch: only dispatch compute for allocated bricks
4. Bake shader writes to atlas texture at brick positions (already does textureStore)
5. Empty bricks stay at +999.0 (already cleared by allocate())
6. VP shader unchanged — reads atlas texture as before
