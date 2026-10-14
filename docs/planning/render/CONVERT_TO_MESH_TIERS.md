# Convert to Mesh — bring the decimation work to the surface as user-facing tiers

**Date:** 2026-07-30
**Owner:** *"the convert to mesh, we need to bring the optimisation window. We built a bunch of
decimation parameters trying to make the DC mesh faster — we could use it tiered to help the user
control output of the SDF to mesh."*

**Status:** design + audit. No code changed.

---

## 1. The owner is right, and the gap is bigger than "no dialog"

**Convert to Mesh today takes no parameters at all.** `convert_sdf_to_poly(nid)` (`convert.rs:43`) is
called straight from the context menu (`outliner_panel.rs:120`, `ui2/outliner/actions.rs:156`) with
nothing but a `NodeId`.

Two consequences, both measured from the code:

1. **Resolution is whatever the viewport happened to bake.** `convert.rs:157-221` reads `res` from the
   *display* snapshot (`display_slot_snapshot_ok`, `cpu_oracle_mesh(..., res)`). So the mesh you get
   depends on where the camera was and what the interaction policy had latched — a draft 64³ preview
   converts to a draft-resolution mesh. The user has no way to say "I want the good one."
2. **Decimation is never applied.** Grep of `convert.rs` for `Decimate` → **zero hits**. The full
   14-parameter, 5-mode decimation system in `core_math/src/display_mesh_ops.rs` is completely
   unreachable from the one operation that most needs it.

So the raw DC output ships as-is: uniform triangle density, no crease protection, no flat-melting.
That is the worst of both worlds — dense where it doesn't matter (flats) and no denser where it does
(edges).

---

## 2. What already exists (this is why the owner's idea is cheap)

`DecimateParams` (`display_mesh_ops.rs:135`) is a mature, well-documented struct — **14 fields**:

| Field | Purpose |
|---|---|
| `ratio` | keep-fraction of triangles (0.05..=1.0) |
| `mode` | `Fast` / `Quality` / `Field` / `Planar` / `Master` (default `Master`) |
| `crease_angle_deg` | dihedral crease threshold (default 40°) |
| `crease_strength` | how hard to protect creases (0 / 1 / 2) |
| `boundary_strength` | protect open boundary edges |
| `field_strength` | SDF curvature weight (Field mode) |
| `field_radius` | SDF sample radius × avg edge |
| `max_error` | QEM error stop (world² ) |
| `planar_bias` | melt flats first (0 / 1 / 2.5) |
| `edge_shape_strength` | AZRA anisotropic-curvature edge protect |
| `flat_starve` | flood tri budget into edge bands |
| `soft_feature_world` | soft-K / bevel band thickness |
| `min_edge_len` | kill dense zippers on straight runs |
| `curve_angle_deg` | keep verts where the chain turns |
| `use_edge_length_field` | target edge-length field ℓ(x) |

`DecimateMode::Master` is already the default and is described as *"mesh importance map + QEM + strong
flat-first"* — i.e. the good one is already written and already chosen.

**14 parameters is exactly why tiers are the right UI**, not a parameter wall. A right-click →
Convert is a 1-second action; nobody is going to tune `soft_feature_world` there. Tiers give the 95 %
case one click and leave the full set for the people who want it.

---

## 3. Proposed design — tiers, with an escape hatch

### The window

Convert to Mesh opens a small modal (it is a destructive, one-shot conversion — a confirm step is
appropriate and currently missing):

```
Convert to Mesh — Body.001

  Quality   ( ) Draft      ~5k tris    fast, blocky
            (•) Standard   ~40k tris   recommended
            ( ) High       ~150k tris
            ( ) Maximum    no decimation (raw DC)
            ( ) Custom…    → expands the full DecimateParams panel

  Bake resolution   [ 128³ ▾ ]     (currently: 128³ display)
  Estimated output  ~38,400 tris

  [ Convert ]  [ Cancel ]
```

### Tier → parameters

The tiers are **presets over the existing struct** — no new math:

| Tier | `mode` | `ratio` | `planar_bias` | `edge_shape_strength` | `flat_starve` |
|---|---|---|---|---|---|
| **Draft** | `Fast` | 0.10 | 1.0 | 0 | 0 |
| **Standard** | `Master` | 0.35 | 1.5 | 1.0 | 0.5 |
| **High** | `Master` | 0.70 | 1.0 | 1.0 | 0.25 |
| **Maximum** | — | 1.0 | — | — | — (skip decimation entirely) |
| **Custom** | user | user | user | user | user |

*Numbers are a starting point, not a measurement.* They must be tuned against real bodies before
shipping — see §5. `Field` mode is worth considering for Standard/High since convert has the
`BodyField` available (`bake_body_grid_field` is right there at `convert.rs:211`), and Field mode is
specifically the one that uses SDF curvature to keep edges dense. **Measure Master vs Field on a
booleaned body before choosing.**

### Resolution belongs in the same window

This is the half the owner did not ask for but is the same bug: the user cannot currently choose the
bake resolution either, and it silently inherits the viewport's. Putting both controls in one window
makes the real tradeoff visible — **resolution controls how much detail exists, decimation controls how
many triangles carry it.** Those are orthogonal and both matter.

Default the resolution field to the current display res so the existing behaviour is one Enter away.

---

## 4. Why this is more than convenience

Three things this fixes that are currently broken-by-omission:

* **The "faster DC mesh" work becomes reachable.** The owner's framing is exact: these parameters were
  built to make DC meshes faster and they are sitting unused behind the one operation that produces a
  permanent mesh. A perf feature nobody can invoke is not a perf feature.
* **Convert is destructive and currently unconfirmed.** It replaces an `SdfBody` payload with a
  `PolygonalBody` (`convert.rs:292` drops the GPU-resident SDF mesh). A one-click irreversible
  conversion with no preview and no options is a UX hazard independent of decimation.
* **It makes output predictable.** Right now the same body converts to different meshes depending on
  camera history. That is the "lying control" class this project has been fixing all week — the menu
  item promises "Convert to Mesh" and delivers "convert at whatever resolution you happened to be
  looking at."

---

## 5. Gates — and one specific trap

**The tier numbers must be measured, not assumed.** The table in §3 is a guess. Before shipping:

* Convert 3 representative bodies (a hard-edged boolean, a soft-K/beveled body, an organic blob) at
  each tier and record **tri count + wall time + a visual check**. Tier presets that look good on a box
  and destroy a bevel are worse than no tiers.
* **Assert monotonicity:** `Draft ≤ Standard ≤ High ≤ Maximum` in triangle count, for every test body.
  A preset table is easy to get non-monotonic by accident, and that is confusing in a way users report
  as "the tiers do nothing."
* **`Maximum` must be bit-identical to today's output.** That is the regression guard: if `Maximum`
  differs from current Convert, the refactor changed behaviour rather than adding options.

**The trap** — from this session's repeated lesson: **do not gate on "the tiers produce different
meshes."** That passes when a tier produces garbage. Gate on the *property*: tri count in the expected
band, mesh still manifold (`validate()` / `is_manifold()` per the poly rules), and creases preserved on
the hard-edged body. This project has now shipped two features whose gate asserted "differs" and passed
on broken output (`p2_vertex_splat…`, and the earlier sabotage-passing tests).

**Also required by the poly rules:** any op that could produce non-manifold or orphaned-twin topology
must assert against it, and `cargo test -p core_math` after touching mesh code.

---

## 6. Scope note — do this AFTER the node-graph correctness work

`PLAN_FOR_GROK_NODE_CORRECTNESS.md` G1–G4 are landed; **G5 (triplanar 3×) and G6/G7 are still open**,
and G6 is the one the owner has reported blocked twice. This convert window is a feature, not a
correctness fix, so it queues behind those.

It is also **not** a candidate for the same session as O5 — Grok just made convert async
(`c813260`, `poll_convert_sdf_to_poly` + `PendingConvertSdf`). Adding a parameter window means the
queued request must now carry the tier/resolution through that async boundary. That is straightforward
but it is a real interaction with fresh code: read `convert.rs`'s new queue/poll/finish split first,
and put the params on `PendingConvertSdf` rather than reading them at finish time (reading them late
would let a user change the tier mid-conversion and get a mesh that matches neither).
