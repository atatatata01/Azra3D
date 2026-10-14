# OCCT oracle harness — reference data for NURBS fillets

**Purpose:** give the fillet a ground truth, the way `docs/planning/poly/*_ref.py` gave the poly tools
one. **Oracle ≠ dependency** — generate offline, commit the JSON, ship nothing.

---

## 1. Which tool — use FreeCAD, not pip

| Option | Verdict |
|---|---|
| **FreeCAD `freecadcmd`** | ✅ **Use this.** Ships its own Python **and** OCCT — no wheel/ABI problem, one download, headless out of the box. |
| `cadquery` / `build123d` (pip) | ⚠️ Nicer API, but both need `cadquery-ocp`, and **this machine runs Python 3.14** — far ahead of available OCP wheels. Would need a separate 3.11/3.12 env first. |
| pythonOCC (conda) | ⚠️ Works, but adds a conda install to the toolchain. |
| OCCT C++ direct | ❌ Heaviest path, no benefit here. |

FreeCAD is Part-workbench-thin over OCCT: `Part.makeBox().makeFillet()` **is** `BRepFilletAPI_MakeFillet`.
Same kernel, none of the packaging pain.

---

## 2. ⚠️ Compare by SAMPLING, not by B-rep structure

**Do not try to match OCCT's faces one-for-one against AZRA's patches.** OCCT uses trimmed analytic
surfaces (`Plane`, `Cylinder`, `Sphere`, `Torus`); AZRA uses tensor-product NURBS patches. Both can be
**geometrically identical while structurally different** — a face-for-face diff would report failures
that are not failures, which is worse than no oracle at all.

Compare what the user actually sees: **the surface**.

### Tier 1 — topology summary (cheap, catches decomposition errors)

A tiny JSON: **face count grouped by surface type**.

```json
{ "case": "box_1x1x1_fillet_r0.2_edges_[0,4]",
  "faces": { "Plane": 6, "Cylinder": 2, "Sphere": 0, "Torus": 0 }, "total": 8 }
```

This is the tier that answers the questions that have been guessed wrong twice:
*is there a corner patch when only 2 of 3 edges are filleted?* — OCCT's count says yes or no,
definitively. **Run this tier first; it is a few KB and it fails fast.**

### Tier 2 — reference mesh + Hausdorff (thorough, catches geometry errors)

Tessellate the OCCT solid finely, commit the mesh, and compare against AZRA's tessellation with a
**two-sided Hausdorff distance**:

* `max over AZRA points of dist(p, OCCT_mesh)` — catches AZRA bulging outside truth
* `max over OCCT points of dist(p, AZRA_mesh)` — **catches gaps and holes**, i.e. the dark wedge and
  the missing corner patch

Both must be < tol (start `1e-3` for a unit box). One-sided is not enough: a mesh with a hole in it
scores perfectly in the first direction.

---

## 3. The harness

```python
# occt_fillet_ref.py — run with:  freecadcmd occt_fillet_ref.py
import FreeCAD, Part, json, os

OUT = os.path.join(os.path.dirname(__file__), "refs")
os.makedirs(OUT, exist_ok=True)

def surface_type(face):
    t = face.Surface.TypeId          # e.g. "Part::GeomCylinder"
    return t.rsplit("Geom", 1)[-1]   # -> "Cylinder"

def emit(name, shape, dev=1e-3):
    counts = {}
    for f in shape.Faces:
        k = surface_type(f)
        counts[k] = counts.get(k, 0) + 1
    verts, tris = shape.tessellate(dev)
    json.dump({
        "case": name,
        "faces": counts,
        "total": len(shape.Faces),
        "vertices": [[round(c, 6) for c in v] for v in verts],
        "triangles": [list(t) for t in tris],
    }, open(os.path.join(OUT, name + ".json"), "w"), indent=1)
    print(name, counts, len(verts), "verts")

def box_fillet(name, r, edge_idx=None):
    box = Part.makeBox(1, 1, 1)
    edges = box.Edges if edge_idx is None else [box.Edges[i] for i in edge_idx]
    emit(name, box.makeFillet(r, edges))

# ── CASE MATRIX (16) — see FILLET_EDGE_CASES.md Tier 0 + Tier 1 ──
# A cube alone cannot produce OnSame/OnDiff — need L-bracket + boss-on-plate.

# Box / AllSame (cube corners only)
box_fillet("single_edge_r02",       0.2, [0])       # 0.1
box_fillet("two_edges_at_vertex",   0.2, [0, 4])    # 0.5 PerformTwoCorner
box_fillet("three_edges_at_vertex", 0.2, [0, 4, 8]) # 0.3 PerformThreeCorner AllSame
box_fillet("one_edge_at_vertex",    0.2, [0])       # 1.1 free ends (same edge set as single; document intent)
box_fillet("all_edges_r005",        0.05)
box_fillet("all_edges_r02",         0.2)
box_fillet("all_edges_r045",        0.45)           # 1.8 near-max
box_fillet("short_edge_overlap_r",  0.4, [...])     # 1.7 setbacks overlap — pick short edge after map

# L-bracket — produces OnSame / mixed concavity at re-entrant vertex
l_bracket_fillet("lbracket_onsame_corner", r=0.15, edges=...)   # 1.2
l_bracket_fillet("lbracket_ondiff_corner", r=0.15, edges=...)   # 1.3 (if geometry allows)
l_bracket_fillet("lbracket_concave_edge",  r=0.15, edges=...)   # 1.4

# Boss-on-plate — second solid for OnSame/OnDiff + closed loops later
boss_fillet("boss_onsame_corner", r=0.1, edges=...)             # 1.2
boss_fillet("boss_plate_junction", r=0.1, edges=...)            # mixed convex/concave

# Dihedral / radius variants (box or L)
box_fillet("unequal_r_at_vertex", ...)   # 1.10 — when FreeCAD exposes SetRadius per vertex
# acute/obtuse: build a wedge solid, not a box (1.5, 1.11)
```

**Solids that must exist in `occt_fillet_ref.py`:**
1. **Unit box** — `AllSame` only (current).
2. **L-bracket** — two boxes fused into an L (re-entrant edges → `OnSame` / `OnDiff`).
3. **Boss-on-plate** — plate + smaller box fused on top face (junction ring + mixed concavity).

**Edge indices are OCCT's ordering, not AZRA's.** Map by midpoint only.

### Coordinate frames (critical)

| Frame | Up | Unit cube |
|---|---|---|
| **AZRA** | **+Y** | centered `[-0.5, 0.5]³` |
| **FreeCAD `Part.makeBox`** | **+Z** | `[0, 1]³` |

```
azra_to_occt: (x, y, z) → (x+0.5, z+0.5, y+0.5)
occt_to_azra: (x, y, z) → (x-0.5, z-0.5, y-0.5)
```

Rust: `azra_to_occt_box` / `azra_edges_from_occt_mids` / `cube_top_loop_edges`.  
FreeCAD: `occt_edge_map.py` → `refs/edge_map_unit_box_corrected.json`.

**Never** use FreeCAD edge `i` as AZRA `(face, edge)` and **never** compare with a pure `+0.5` shift.

---

## 4. AZRA side

1. Run the same op through `apply_edges_fillet_batch`.
2. **Tier 1:** classify each resulting face (planar / cylindrical / spherical) and compare the counts.
   A mismatch means the *decomposition* is wrong — stop and fix that before looking at geometry.
3. **Tier 2:** tessellate, compute two-sided Hausdorff against the reference mesh, assert < tol.
4. Commit the `refs/*.json` next to the tests. **Regenerate only when the case matrix changes** — never
   to make a failing test pass.

---

## 5. Rules for this harness

* **Never regenerate a reference to silence a failure.** The reference is truth; AZRA is what changes.
  This is the single way an oracle becomes worthless.
* **Cube refs never validate `OnSame` / `OnDiff`.** If the case needs mixed concavity, the solid must be
  L-bracket or boss-on-plate — do not "approximate" with a cube edge set.
* **Tier 1 before Tier 2, always.** Polishing surface geometry while the face count is wrong is wasted
  work — both fillet attempts so far failed at Tier 1.
* **Run the whole matrix, not one case.** The near-max-radius row exists because the defect *scales*
  with radius; small-radius-only testing is how it survived two rounds.
* Keep the analytical invariants (§0) as well — they are faster, exact, and need no tooling. The oracle
  answers *what to build*; the invariants verify *that it was built right*.
* Note the FreeCAD version in each JSON. If a future version changes tessellation, a diff should be
  explainable rather than mysterious.
