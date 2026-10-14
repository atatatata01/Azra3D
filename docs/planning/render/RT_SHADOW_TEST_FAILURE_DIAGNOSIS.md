# RT-shadow GPU test failures — diagnosis

**Date:** 2026-07-28
**Scope:** diagnosis only, no behaviour changes (as scoped).
**Status:** root cause identified with certainty. **Production is NOT affected.**

## Symptom

Four tests in `graphics` fail, all with the same wgpu validation error:

```
test rt_shadow_gpu_test::rt_shadow_gpu_occlusion_matches_cpu_oracle ... FAILED
test rt_shadow_gpu_test::rt_shadow_per_light_radius_widths ... FAILED
test rt_shadow_gpu_test::rt_shadow_per_light_disable_skips_light ... FAILED
test rt_shadow_gpu_test::rt_shadow_soft_penumbra_and_contact_hardening ... FAILED

[RT-SHADOW-GPU] wgpu error: Validation Error
  In Device::create_bind_group, label = 'rt_shadow_bg_test'
    Number of bindings in bind group descriptor (11) does not match the
    number of bindings defined in the bind group layout (12)
```

The rest of the crate is green (65 passed, 4 failed).

## Root cause

`f42a317` ("feat(pbr-lights): P2.2 dual RT packs — 8 lights (matches deferred cap)") added a
second visibility output — **binding 28**, the pack1 storage texture for lights 4..7 — to the
shared layout in `graphics/src/bgl.rs`:

```rust
pub fn rt_shadow_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![
        sto(0, TF::Rgba16Float), uni(1), tex(12), uni(25), buf_ro(26),
        sto(28, TF::Rgba16Float), // P2.2 pack1 (lights 4..7)   <-- added by f42a317
        buf_ro(42), buf_ro(43), buf_ro(44), buf_ro(45), buf_ro(46), uni(47),
    ]
}
```

That commit updated the production bind group (`app/src/gpu_init.rs:748` `build_rt_shadow_bg`),
which binds all twelve:

```
0, 1, 12, 25, 26, 28, 42, 43, 44, 45, 46, 47      <- production, complete
```

It did **not** update the test's bind group (`graphics/src/rt_shadow_gpu_test.rs:310-320`),
which still binds eleven:

```
0, 1, 12, 25, 26,     42, 43, 44, 45, 46, 47      <- test, missing 28
```

The layout is shared (single source, correctly), so the test's stale entry list no longer
matches it and `create_bind_group` rejects the descriptor before any shader runs.

## Why production is fine

This is worth stating explicitly, because "four GPU shadow tests fail" reads much worse than it
is. The BGL is built from `rt_shadow_bgl_entries()` at both sites — there is no second copy of
the layout to drift. Production's entry list was updated in the same commit that changed the
layout, and it binds all twelve. **Shipping RT shadows are unaffected; only the test's bind
group is stale.**

The corollary is that these four tests have been proving nothing since `f42a317` — they fail in
`create_bind_group`, so none of the occlusion, penumbra, per-light-radius or per-light-disable
assertions has executed since then.

## Fix (not applied — out of the diagnosis-only scope)

Add the missing entry to the test's bind group, alongside a pack1 texture + view created the same
way `out_view` already is:

```rust
wgpu::BindGroupEntry {
    binding: 28,
    resource: wgpu::BindingResource::TextureView(&out_hi_view),
},
```

After that the four assertions run again for the first time since `f42a317`, so expect to
actually read their output rather than assuming green.

## Class of bug

This is the failure mode recorded in the `feedback_bgl_binding_sites` memory: adding a shader
binding requires updating every bind-group construction site, and a missed one fails at the
validation layer rather than at compile time. The single-source-layout rule (CLAUDE.md, "BGL
Single Source") is being followed correctly and is what made this diagnosable in minutes — but
it only unifies the *layout*, not the *entry lists*, and nothing structurally forces those to
agree.

Worth considering (not proposed as part of this task): a helper that builds the entry list from
the same table as the layout, so a new binding is a one-line change instead of an N-site one.
That is the only version of this that stops the class rather than the instance.
