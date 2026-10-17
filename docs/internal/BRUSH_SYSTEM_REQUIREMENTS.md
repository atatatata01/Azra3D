# Brush System — Requirements (the spec the behavior-stack foundation must satisfy)

Consolidated from the owner's accumulating asks (2026-06-28). Pairs with `BRUSH_SYSTEM_ARCHITECTURE.md`
(the architecture, produced by the design workflow). This file is **what** the system must do; the
architecture doc is **how**. The north star (owner's words): *"a stack of behaviors that lets a user
create unique brushes."*

---

## 1. Composable behaviors (the "stack")
- A brush = a **base operator** + zero or more **added secondary behaviors**, each independently toggled.
- Example asks: **Pinch** must be an *addable option* on any brush (not only a standalone brush), with its
  **own strength** and **pressure** control. The same must hold for Smooth, Inflate, Color-paint, etc.
- The current 11 brushes (Draw, Erase, Smooth, Flatten, Clay, Scrape, Crease, Fill, Build, Inflate, Pinch)
  + **Move/Grab** become **presets** = canonical stacks, not hardcoded enum-only behavior.

## 2. Per-parameter modulators (brush dynamics) — THE jitter/pressure matrix
**Every** numeric brush parameter carries: a **base value**, an optional **pressure mapping**, and an
optional **randomization (jitter)** with its **own strength/amount slider**. This is the core ask
("random size, random rotation, random strength… all controllable by pen pressure and with their own
strength slider").

| Parameter            | Base | Pressure map | Jitter (own amount slider) |
|----------------------|:----:|:------------:|:--------------------------:|
| Size / Radius        |  ✔   |      ✔       |             ✔              |
| Strength / Flow      |  ✔   |      ✔       |             ✔              |
| Rotation             |  ✔   |      ✔       |             ✔              |
| Spacing / Scatter    |  ✔   |      ✔       |             ✔              |
| Falloff hardness     |  ✔   |      ✔       |             ✔ (stretch)    |
| Per-behavior strength (e.g. Pinch amount) | ✔ | ✔ | ✔ |

- **Pressure map** = a curve from pen pressure (0..1) → a multiplier on the parameter. (Pressure source
  caveat: real Windows tablet-pen pressure needs a wintab/Windows-Ink integration — see the pressure
  feature's report; touchscreen works today; mouse = 1.0 fallback.)
- **Jitter** = per-stamp randomization by ±amount, from a **seeded deterministic RNG** (must be
  reproducible for undo/redo — never `rand()` ad hoc). Each parameter's jitter has its own 0..1 amount.
- Modulators compose: a parameter can be **both** pressure-mapped and jittered (e.g. radius = base ×
  pressure × (1 ± jitter)).

## 3. Direction / Sculpt-plane (in progress)
- Modes: **View** (camera), **Surface** (live normal — today's default), **FirstPoint** (normal latched at
  stroke start = Blender/ZBrush "Original Normal"), **X / Y / Z** axis locks. (Implementing now.)

## 4. Falloff (shipped)
- Curve control: **Smooth / Linear / Sharp / Constant** (extensible). Drives every brush. Works on the
  GPU default path.

## 4b. Brush Alpha / Detail — PNG stamp
- A **PNG image** drives the brush's spatial intensity (the "alpha" / "stamp" / "detail" — ZBrush alphas,
  Photoshop brush tips, Blender texture brushes). Per footprint point: `weight = falloff(r) × alpha_png(uv)`.
- **UV** = the footprint position mapped into the brush's **oriented local frame** — so the alpha is rotated
  by the **Rotation** parameter (and its jitter, §2) and oriented by the **Direction** plane (§3). This is
  why alpha, rotation-jitter, and direction are one coherent system, not three bolt-ons.
- Greyscale = intensity (0 = no effect, 1 = full). Optional: use the PNG's alpha channel; tile vs.
  single-stamp; invert; contrast. Single-stamp (one alpha per dab, advanced by Spacing §2) is the priority;
  drag/roll modes are a stretch.
- **Loading**: decode the PNG (the `image` crate / `io_bridge`), upload as a GPU texture for the GPU brush
  kernel; keep a CPU copy for the CPU `apply_brush` path. Generation-counted like every GPU resource.
- **GPU execution**: the kernel samples one bound alpha texture — a uniform/bindless slot, NOT a per-alpha
  shader variant (no shader explosion). The CPU path bilinear-samples the CPU copy.
- **Library**: alphas live in a user folder + a built-in set; a custom brush references its alpha by id so a
  saved preset reloads it (serialization, §Execution).

## 5. Channels / what a stamp writes
- Density add / density subtract / **per-voxel color (paint)** / **displacement (advect = Move)**.
- Paint-color caveat: currently CPU-path-only; with GPU-default-on it needs the "use CPU path when the
  behavior requires it" rule (Move already self-forces CPU — the proven pattern).

## 6. Global stroke modifiers (exist, keep)
- Symmetry (X/Y/Z), Lazy-mouse/steady-stroke, per-frame budget cap.

---

## Execution constraints (non-negotiable, from CLAUDE.md)
- **GPU-authority**: the sparse atlas is truth during a stroke; the brush must `mark RENDER` for live update.
- **Dual path**: every behavior runs on the CPU `apply_brush` AND the GPU brush path — OR self-forces CPU
  (like Move) when it has no GPU op. GPU paths are **default-on**. No per-combination shader explosion / TDR
  (Naga inlines WGSL) — the architecture doc must name the GPU execution strategy.
- **Undo**: sculpt undo is its OWN snapshot/delta path (NOT `transaction()`). Jitter RNG must be seeded so
  a stroke is reproducible.
- **Serialization**: custom brushes + presets are saveable (a brush library); save-format versioned.

## Acceptance test (the worked example the foundation must make trivial)
"A **clay** brush that also **paints red**, follows the **surface normal**, with **pressure-driven radius**,
**±20% size jitter**, **random rotation**, and a **sharp falloff** — built by the user from the panel,
saved as a preset, and replayable with identical results under undo/redo."

---

## Status / sequencing
- **Shipped**: Falloff, Move/Grab, paint-color (needs CPU-reconcile under GPU-default), pressure (plumbing;
  source limited on Windows tablet), GPU-default.
- **In progress**: Direction modes.
- **Designing**: the behavior-stack architecture (workflow) → becomes `BRUSH_SYSTEM_ARCHITECTURE.md`.
- **Then**: build the stack foundation covering §1–§2 (behaviors + the pressure/jitter modulator matrix) —
  Pinch-as-option and brush-dynamics land as the first compositions, not new bespoke code.
- **Bottleneck note**: §1–§5 all live in `voxel_sculpt.rs` / `input_sculpt.rs` / `ui_panels.rs`, so feature
  work there is **serialized** (one agent/edit at a time) — the foundation exists partly to end that.
