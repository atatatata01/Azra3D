# Volumetric Lights (God Rays / Light Shafts)

Status: **Phase 1 SHIPPED (uncommitted, 2026-07-18)**. Phase 2 (draggable viewport handles) PARTIAL — read-only visualization ships, drag interaction is the follow-up.

Owner ask (verbatim): *"lights need a setting to turn them into volumetric, with viewport handles to control them — softness, zone."*

Interpretation (as built):
- **Volumetric** = per-light master toggle for volumetric in-scatter.
- **Zone** = the scattering reach in world units (how far shafts travel / point-spot volume radius). `0` = use the light's `range`.
- **Softness** = beam-edge feather `0..1` (widens the cone/sphere falloff band).
- **Density** = scattering strength of the light's own participating medium (`0` = none, `1` = default).

---

## Architecture — reuse, not a new system

The volumetric-light effect is the SAME ray-march-with-shadow math the fog "god ray" pass already does, scoped **per light**. It was implemented by **extending the existing fog pass** (`graphics/src/fog_entry.wgsl`), NOT a second volumetric renderer. It reuses `fog_shadow()` (SDF volumetric shadow march) and `mie_phase()` (HG phase) unchanged.

Per light type:
- **Directional** → atmospheric shafts (phase halo + shadowed beams through gaps). Zone is non-spatial (shafts span the march).
- **Spot** → the classic light cone. `spot_outer`/`spot_inner` gate the cone; softness pushes the outer edge outward and feathers the rim. Zone = cone length / distance falloff.
- **Point** → glow sphere within the zone radius; softness feathers the radial edge.

Composite: additive in-scatter accumulated in the fog texture, composited **before tonemap** (same ordering as fog/SSR), gated by `fog_or_vol` (bit 8 of `bloom_flags`). Never double-counts direct light; SSAO/GTAO do not touch it. Budget: the vol loop runs inside the existing fog march (shared step positions + transmittance), so it adds `num_steps × num_vol_lights × shadow_steps` `scene_sdf` evals — `scene_sdf` is O(1) via the voxel cache. Skipped entirely during interaction episodes (`!interaction_active`).

The fog/vol pass runs when **global fog is on OR any visible light is volumetric** (`SceneState::any_volumetric_light()`), so volumetric lights show even with fog off — each volumetric light carries its OWN medium (its `Density`), independent of `fog_density`.

---

## Persistence — FL4LTVOL tail (the anti-field-trap mechanism)

`LightData` is bincode-serialized inside the scene graph inside the `.fl4` blob. Adding a naked field to it would corrupt EVERY existing `.fl4` (the confirmed bincode field trap). So:

- The four params live in `core_math::scene_graph::LightVolumetric`, carried on `LightData` behind **`#[serde(skip)]`** → **zero wire bytes → the bincode blob stays byte-identical** to every pre-change `.fl4`.
- They persist OUT OF BAND in a NEW self-delimiting **`FL4LTVOL`** tail section (`project.rs`), keyed by light `id` — mirroring the `FL4ENVIR` precedent exactly. Only lights with `enabled == true` emit a record, so a scene with no volumetric lights writes a file with **no `FL4LTVOL` magic** (byte-compatible with older saves).
- Written BEFORE the `FL4ENVIR` section so `FL4ENVIR` stays the LAST tail section (older-reader tolerance). Reattached to graph lights by id on load, after `heal_duplicate_light_ids()`.

**Ref-compat proof:** `dc_gate0_committed_ref_fl4_loads_unchanged` (runs the real loader over the committed reference `.fl4`) stays GREEN — the pre-change file loads unchanged. New round-trip gates in `project::light_vol_save_tests` prove the params survive save/load and that absence writes no magic. `serialize_to_gpu_volumetric_layout` (core_math) locks the 20-float CPU→GPU contract.

Note: the JSON debug save path (`save_json`) does not carry `#[serde(skip)]` fields — same known limitation as `child_orders`/`env`/`material_maps`. `.fl4` (binary) is the authored format.

---

## GPU light buffer stride 16 → 20

Each light serializes to 20 floats (`LightData::serialize_to_gpu`): floats 0..15 base, 16..19 = `[enabled?1:0, density, softness(clamped), zone_effective]` (zone `0` resolves to `range` at serialize time). `FLOATS_PER_LIGHT` (sdf_lighting.wgsl) and the `VL_STRIDE` mirror in fog_entry.wgsl were bumped to `20u`. The fog BGL + bind group gained **binding 26 (light_data)** at all three sites: `app_new.rs` (`fog_bgl`), `app_update.rs` (`bg_fog_bgl`, background thread), and the `fog_bg` bind group in `viewport_render.rs`. The `light_buffer` reallocs on growth and `fog_bg` is recreated in the same block as `deferred_bg` (nulled on realloc), so no reference dangles.

---

## Files touched (Phase 1 + partial Phase 2)

- `core_math/src/scene_graph.rs` — `LightVolumetric` struct; `LightData.volumetric` (`#[serde(skip)]`); `serialize_to_gpu` → `[f32;20]`; layout unit test.
- `graphics/src/sdf_lighting.wgsl` — `FLOATS_PER_LIGHT` 16 → 20.
- `graphics/src/fog_entry.wgsl` — `vol_light_inscatter()` + accessors; per-light in-scatter accumulated in the march loop.
- `app/src/scene_actions.rs` — light buffer capacity hint 16 → 20.
- `app/src/scene_state.rs` — `any_volumetric_light()`.
- `app/src/project.rs` — `LightVolRecord`; `ProjectFile.light_volumetrics` (`#[serde(skip)]`); `FL4LTVOL` magic + save/load sections; round-trip test module; 3 `ProjectFile` test-helper literals.
- `app/src/project_save.rs` — `collect_light_volumetrics()` + `from_app` wiring.
- `app/src/project_load.rs` — reattach by light id after heal; migration-path `LightData` literals.
- `app/src/dc_baseline.rs` — `project_with` helper carries the field.
- `app/src/properties_light.rs` — Volumetric panel section (toggle + Density/Softness/Zone, undo via `begin_episode`).
- `app/src/viewport_render.rs` — fog dispatch + composite gated on `fog_or_vol`; fog_bg binding 26.
- `app/src/app_new.rs`, `app/src/app_update.rs` — fog BGL binding 26 (both sites).
- `app/src/draw_light.rs` — read-only volumetric zone/softness overlay (Phase 2 visualization).

---

## Phase 2 — draggable viewport handles (FOLLOW-UP)

**Done:** a READ-ONLY overlay (`draw_light.rs::draw_volumetric_zone`) draws, for a *selected volumetric* light, a violet Zone reach ring + a fainter softness-feather ring (+ spot beam rails), plus a "VOL" badge on every volumetric light. It is purely a painter draw with `world_to_screen` — no hit-testing — so it CANNOT affect light selection/move (P0 preserved).

**Not done (the actual drag handles):**
- A **Zone handle** (drag a point on the reach ring to set `zone` live) and a **Softness handle** (drag the inner ring to set `softness`).
- Wiring points: hit-test in `light_gizmo_hit_test()` (`draw_light.rs`) — add handle hit-tests that return a handle id, dispatched from the viewport input handler alongside the existing light-gizmo pick; drag through `begin_episode("Edit volumetric zone")` → `write_light(id, |l| l.volumetric.zone = ...)` → `commit_episode()` (ONE undo entry, mirroring the panel sliders). Handles must only appear for volumetric-enabled lights and must NOT shadow normal light selection/move (test selection after wiring — P0).
- All params already work fully via the panel sliders, so the handles are pure convenience.

---

## Owner manual test

1. Dark/foggy scene, add a Spot light, aim it at the floor with an occluder between.
2. In Light properties → Volumetric → enable. A light cone appears; it is bright where unshadowed, dark behind the occluder.
3. Drag Density / Softness / Zone → the cone brightness, edge feather, and reach change live.
4. Select the light → violet Zone/Softness rings show the reach in the viewport (Phase 2 visualization).
5. Save, reload → the volumetric settings are preserved.
6. Toggle Volumetric off → the cone disappears (no scattering).
