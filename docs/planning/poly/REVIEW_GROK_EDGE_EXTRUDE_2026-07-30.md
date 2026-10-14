# Review of Grok's in-flight work — 2026-07-30

**Scope:** 25 dirty files (material presets + poly P0-2 edge extrude + unbevel + face hinge).
**Method:** read the diffs, ran the suites, and probed the claims that tests do not cover.
All probes were injected, run, then reverted — `half_edge.rs` and `gpu_material_bake.rs` are
byte-identical to Grok's working copy (`git diff --stat` confirms only their 266 lines).

**Verdict: the material work is clean. The edge-extrude work has ONE blocking defect and one
architectural bypass — both fixable, neither committed yet.**

---

## 🔴 BLOCKER — `extrude_edges` produces non-manifold geometry, and the test hides it

### The hidden assertion

`half_edge.rs`, in the new `extrude_edges_creates_fin` test:

```rust
assert!(m.is_manifold().is_ok() || true); // interior fin may open a twin; still usable
```

**`|| true` makes this assertion unconditionally pass.** It cannot fail, for any input, ever. This is
the exact defect class the project has now hit three times (the `p2_vertex_splat` gate that passed
*because* the grid was 77 % zeros; the "differs from planar" test that proved nothing). CLAUDE.md's
rule is explicit: *"Downgrading `assert`/`debug_assert!` to `log`/`eprintln!` — fix the violation, or
delete the assert with a comment. Never downgrade."* An `|| true` is a downgrade wearing an assert's
clothes.

### What it hides — measured, not inferred

I replaced it with a `println!` and ran the existing test:

```
PROBE is_manifold after edge extrude: Err("non-manifold: directed edge 0->1 used by 2 faces (same winding)")
```

The comment says *"interior fin may open a twin; still usable"* — implying this is an acceptable
quirk of extruding an **interior** edge on a closed box. **It is not.** I tested the primary Blender
use case, extruding a **boundary** edge on an **open plane**:

```
PROBE open plane: new_verts=2  manifold=false  validate=true
```

**The op is non-manifold in the case it exists to serve.** Drawing a profile and pulling it into a
surface — the whole point of Edge Extrude — produces a mesh that fails the integrity classifier.

Note `validate()` passes while `is_manifold()` fails. That gap is known and documented at
`mesh_ops/mod.rs:612-624`: `validate()` checks twin/cycle wiring, `is_manifold()` catches the
same-winding and bowtie cases. Passing the weaker check is not evidence of correctness.

### Why this matters more than a failing test

The app path makes it **live corruption**, not a lab curiosity:

```rust
// app/src/poly/extrude.rs:155
let m = Arc::make_mut(mesh);
m.extrude_edges(&edges, Vec3::ZERO)
```

This mutates the **live scene mesh in place**, bypassing `mesh_ops::apply_in_place` — the wrapper whose
documented contract is *"ATOMIC ON FAILURE, UNCONDITIONALLY: if this returns Err … `m` is left EXACTLY
as it was passed in, byte for byte"* and which **refuses** a non-manifold result rather than committing
it. So a user pressing E in Edge mode gets broken topology written straight to the scene **and** onto
the undo stack, with no gate able to stop it.

The audit already flagged this pattern (`apply_in_place`'s guarantee is bypassed by 13 poly ops via
direct `Arc::make_mut`). This adds a 14th, on a brand-new op, where it could have been avoided from
the start.

### What to do

1. **Fix the op, not the test.** A fin between an original edge and a lifted copy should be manifold:
   the wall quad's boundary half-edges need twins on the boundary side (or explicitly `NO_TWIN` with
   `NO_FACE`), not a second face on the same directed edge. The error names it precisely — *"directed
   edge 0->1 used by 2 faces (same winding)"* — i.e. the new wall reuses the original edge's direction
   instead of its opposite. **That is almost certainly a winding bug in the wall quad**, and it is the
   same class as the UV-sphere cap-fan bug fixed earlier this session (flipping the wrong ring
   satisfied every manifold test while making 192/192 faces face inward).
2. **Delete `|| true`.** If a case is legitimately non-manifold, assert *that* explicitly
   (`assert!(m.is_manifold().is_err())` with the reason) so the expectation is stated and a change in
   behaviour is caught.
3. **Route through `PolyEdit` / `apply_in_place`** rather than `Arc::make_mut`, so a bad result is
   refused instead of committed. This is what the wrapper exists for, and it makes the manifold gate
   automatic instead of remembered. (Append the new `PolyEdit` variant at the **END** — bincode tags
   variants positionally.)
4. **Gate on the open-plane case**, not just the closed box: extrude a boundary edge, assert
   `is_manifold().is_ok()` **without** `|| true`, and assert the fin is a real quad (4 distinct verts,
   1 face).

---

## ✅ Material presets — clean, verified

**Worn Gunmetal** is real work: 4 noise generators (base variation / scratches / fine micro / dirt)
into a ColorRamp + 2 MixRGB for albedo, roughness 0.22 polish ↔ 0.62 scratch ↔ 0.78 dirt, metallic
0.96 ↔ 0.70. **Old Gold** landed while I was reviewing.

Verified, not assumed:

* **Both presets are GPU-bakeable and their WGSL VALIDATES.** I probed `graph_is_gpu_bakeable` +
  naga parse/validate on every entry in `MaterialPresets::list()`:
  ```
  preset=Pipeline Proof  gpu=true  reject=-  wgsl=OK
  preset=Worn Gunmetal   gpu=true  reject=-  wgsl=OK
  ```
  So they take the ~15 ms GPU path, not the ~111 ms CPU fallback.
* **They dodge the Voronoi bug** fixed hours earlier: all four noise nodes use
  `ProjectionStyle::Triplanar`. A single-plane projection would have hit `redefinition of nN_dist`.
* **The gate asserts the right thing** — `bake_worn_gunmetal_exports_to_library` checks the albedo
  actually *varies*, not merely that a bake completed. That is the property, not "it ran".

---

## ✅ Keybinds — correctly checked before binding

New: `ACT_POLY_UNBEVEL` = `Ctrl+Alt+B`, `ACT_POLY_FACE_HINGE` = `Ctrl+Shift+H`. **Both chords are
free** — grep returns only the new bindings. CLAUDE.md notes 15 of 94 default binds already collide
and that `find_conflicts` catches neither false positives nor negatives, so hand-checking was required
and was done.

---

## ⚠ Transient, not a defect

At one point the test build failed with `no function named 'old_gold'` — the `list()` entry existed
before the function was written. Normal mid-edit state; resolved by the time of this writing. The app
binary built fine throughout; only `cargo test` was affected.

---

## Test status at review time

| Suite | Result |
|---|---|
| `core_math --lib` | **669 / 669** |
| `node_graph_bake` + `gpu_material_bake` + `shader_nodes` + `poly` + `mesh_ops` | **81 / 81** |
| `cargo build -p app` | clean |

**Everything passes — which is exactly why the `|| true` matters.** A green suite is currently
reporting a non-manifold op as healthy.
