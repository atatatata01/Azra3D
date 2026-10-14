# SCULPT FEEL — Plan (authoritative for this effort)

Sister-doc to `SCULPT_FEEL_LOG.md` (the running log). This is the roadmap + the oracle-harness
spec. Mirrors the poly effort's `POLY_FOUNDATION_PLAN.md` discipline, adapted for *feel* tuning
of a voxel sculpt system. Branch: `feat/sculpt-tool`.

## 0. The one rule that shapes everything

Blender is the oracle, but **voxel sculpt is compared to Blender by response *behavior*, not by
vertex parity** (see LOG §C). Bevel matches vertices; sculpt matches curves and displacement
fields. Do not build a vertex-parity test against Blender for a voxel brush — it is impossible and
would be a lie. Match: (1) falloff LUTs, (2) displacement-vs-distance fields, (3) behavioral
scalars.

## 1. Red lines (do not cross)

- **CPU/GPU byte-identical.** Falloff and stamp math live in BOTH `core_math/src/voxel_sculpt.rs`
  and `graphics/src/voxel_brush.wgsl`. Every change touches both; the golden CPU/GPU parity test
  must stay green.
- **Display Invariant.** Never fuse raster + raymarch. Feel work does not add a render path.
- **GPU-authority during strokes.** Source of truth mid-stroke is the atlas; don't reintroduce
  CPU authority for sparse strokes.
- **No new serialized fields without the FL4 in-blob `#[serde(default)]` / sentinel discipline**
  (see `project_bincode_save_format_field_trap` memory). A "feel" knob that must persist rides an
  in-blob default field or a settings file — never a naked new field on a bincode struct.
- **One phase per session.** Build + test after every step. Update the LOG after every session.
- **No bandaids** (CLAUDE.md standard). If a feel fix needs a magic constant tuned to one GPU/scene,
  it's not done — derive it or expose it.

## 2. The oracle harness (build once in Phase F1, reuse forever)

Layout under `docs/planning/sculpt/`:
```
oracle/
  falloff_ref.py       # dumps Blender brush.curve presets as strength(t) LUTs -> falloff_*_ref.json
  grab_field_ref.py    # Grab on a flat vert grid -> displacement(dist) -> grab_field_ref.json
  draw_field_ref.py    # Draw dab on flat grid -> height(dist) -> draw_field_ref.json
  spacing_ref.py       # dabs-per-travel + accumulate behavior -> spacing_ref.json
  *.json               # captured reference data (checked in, like the bevel *_ref.json)
  README.md            # how to run: blender --background --python <script> -- <out.json>
```
Rust side: a `sculpt_ref` test module (sibling of poly's `test_spine.rs`) that loads the ref
JSONs and asserts our functions match within epsilon. Runs under `cargo test -p core_math`.

Ref JSON shape (falloff example):
```json
{ "preset": "SMOOTH", "n": 64, "t": [0.0, 0.0158, ...], "w": [1.0, 0.9993, ...] }
```

## 3. Gated phases (each = one session, LOG entry, tests green before advancing)

- **F0 — Harness + orientation.** (this doc + LOG Session 0). No behavior change. Then: build the
  oracle scripts + Rust `sculpt_ref` loader against the *current* `falloff_weight` so the loop is
  proven end-to-end before we change anything. GATE: `cargo test -p core_math` green; a falloff LUT
  captured from Blender and loaded in a Rust test.
- **F1 — Falloff parity.** Reconcile `falloff_weight` (CPU+GPU) to Blender's preset curve(s).
  Decide: match one default (Smooth) or expose the preset set as a Kernel variant. GATE: Rust
  parity test within epsilon of the chosen Blender LUT; golden CPU/GPU parity still green; visual
  A/B in-app.
- **F2 — Grab / Move.** Match Blender Grab's `disp = drag * curve(dist/r)` displacement field
  (and evaluate Elastic-Deform/Kelvinlets as an optional mode). Owner flagged Move as the proven
  reference. GATE: displacement-vs-distance within epsilon of `grab_field_ref.json`.
- **F3 — Stroke spacing / dab model.** Match Blender's spacing (default ~10% radius) + accumulate
  behavior; verify no gaps on fast flicks, no pile-up on slow drags. GATE: dabs-per-travel matches
  `spacing_ref.json`; measured in-app.
- **F4 — Smooth / relax feel.** Match Blender smooth response (iteration/amount → perceived
  strength). GATE: draw+smooth field vs ref.
- **F5+ — Stabilizer (LazyMouse), pressure ramps, symmetry polish** — as separate gated sessions.

Order is a proposal; owner picks the first concrete target (F1 recommended; F2/Grab is the other
strong candidate). Later phases can reorder freely as feel priorities emerge.

## 4. Definition of done (per phase)

1. `cargo build -p app` green; `cargo test -p core_math` green (incl. golden CPU/GPU parity).
2. Rust parity test vs the phase's Blender ref JSON passes within stated epsilon.
3. One in-app A/B manual scenario described (before/after feel), rebuilt binary (STALE-BINARY
   gotcha — always rebuild before trusting a repro).
4. LOG entry written (hypothesis / ref / tried / result / why / kept-or-reverted).
5. `COMPLETE_BUG_TRACKER.md` updated if a bug was found/fixed.
