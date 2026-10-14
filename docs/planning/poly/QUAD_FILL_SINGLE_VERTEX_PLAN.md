# Quad Fill from a Single Vertex (F2-style) — Plan

**Date:** 2026-08-09
**Owner ask:** *"ALTERNATIVE MODE TO FILL WHEN ONE VERTEX IS SELECTED — FILL QUAD USING OPEN
CONNECTED SEGMENT"* — reference image: green circle = the one selected vertex, red = the open
boundary chain around a 4-corner gap.
**Industry reference:** Blender's **F2 addon** (extends `F`: single boundary vertex → quad).
**Status:** Plan only. Nothing implemented.

---

## 0. Owner decisions (answered 2026-08-09 — settled)

| Question | Decision |
|---|---|
| Boundary loop longer than 4 verts | **One quad at the selected vertex, repeatable.** Selection advances so tapping `F` zips the hole shut quad by quad. Never emits an n-gon. |
| Direction disambiguation | **Follow the mouse** — walk toward whichever side the cursor is on. |
| Can it invent a 4th corner? | **Yes — full F2.** When the boundary cannot supply a 4th corner, extrapolate one and grow a quad off the open border. |

---

## 1. Current state (verified)

`poly_fill_face` ([fill.rs:15](../../../core_engine/app/src/poly/fill.rs#L15)) needs **3+**
vertices, or derives 3+ from an edge selection. With exactly one vertex selected it dead-ends at
[fill.rs:42-45](../../../core_engine/app/src/poly/fill.rs#L42):

```rust
if verts.len() < 3 {
    self.toast("Fill: select 3+ vertices, or an edge loop");
    return;
}
```

That is the branch this plan fills in. Everything after it — angular sort, camera-facing winding,
`create_face` — stays exactly as is for the existing paths.

**No new keybinding.** `F` is already `ACT_POLY_FILL`, guard `WHEN_POLY_VERT`
([keybindings.rs:506,701](../../../core_engine/app_types/src/keybindings.rs#L506)). Vertex mode is
already its home; this is a new branch inside the existing handler.

**Reading the mouse is precedented, not a deviation.** `poly_start_inset_drag(mouse)` and
`poly_start_extrude_sweep(mouse)` both read `self.viewport.viewport_mouse` from
`key_dispatch.rs`. The mouse-direction rule uses the same field the same way.

**Do not duplicate Grid Fill.** `Ctrl+Alt+F` / `ACT_POLY_GRID_FILL` already fills a *closed loop*
with a quad grid. This tool is the incremental, single-vertex counterpart — one quad per press. The
two must stay distinct in the menu text and in the toast.

**Keep the refuse-before-transaction discipline.** `try_fill_ring` dry-runs `create_face` on a
clone and returns before opening the transaction, with the comment *"`transaction()` always pushes
— a failed fill must not eat Ctrl+Z."* Every new decline path here obeys the same rule: toast and
return with the mesh untouched, no undo entry.

---

## 2. The traps

### 2.1 `vertex_fan` does NOT give you both boundary edges

[nav.rs:25-41](../../../core_engine/core_math/src/half_edge/nav.rs#L25) breaks the walk on
`twin == NO_TWIN` (line 35, commented `// boundary`). On a boundary vertex the fan therefore
returns only the edges on **one side** of the gap. This op needs **both** boundary edges at `V` —
they are the two directions the user is choosing between.

**Rule:** add a `boundary_edges_at(v) -> (Option<HalfEdgeId>, Option<HalfEdgeId>)` helper next to
`edge_slide_rails` in `nav.rs` — one place, reused by the walk, the direction test and the
extend case. Do not open-code a reverse fan walk at the call site.

### 2.2 Closing vs extending must be *decided*, not guessed

The two modes produce very different results, so the classification has to be explicit:

*(CORRECTED 2026-08-09 — the original wording contradicted §0 and the P2 gate. It said a walk that
fails to close within 3 steps is the extend case, but §0 requires a 6-vertex hole to fill the quad
nearest `V`, and a 6-vertex rim needs 5 steps to close. §0 is the owner's settled decision and
wins.)*

*(CORRECTED AGAIN 2026-08-10, after P4 — the rule below said extend is entered by "exactly one
`Some`". On the owner's ACTUAL shape that is false, and following it made the one case extend
exists for permanently unreachable. See the correction under this list.)*

* `boundary_edges_at(V)` → `(None, None)` = **interior vertex** → decline.
* Exactly one `Some` = **chain end**: the rim terminates, so a 4th corner must be invented →
  one route into the **extend** case (P4).
* Both `Some` → **closing**: walk the rim, capping at 4 corners. Closes at 3 → triangle. Closes at
  4 → quad. Does not close within the cap → still fill the quad **nearest `V`**, leaving the rest
  of the hole open (§0's zip behaviour).

A 3-step closure fills a triangle rather than declining — a 3-vertex gap is a legitimate hole.

### Extend is entered from FOUR closing refusals, not one (P4, 2026-08-10)

On the owner's real shape — a 2×2 grid missing its corner cell, the "L-notch" — `boundary_edges_at`
returns **both** `Some`. The rim walk leaves the notch, comes back around the OUTSIDE, and returns a
ring that closes across an already-interior edge → duplicated directed edge → `ClosingTry::Unsafe`.
So the literal §2.2 rule ("exactly one `Some`") made the single case extend exists for
**unreachable**.

Extend therefore picks up **`ChainEnd | TooShort | Degenerate | Unsafe`**. This is not a
fall-through-on-broken-topology hack: extend proves its own quad from scratch through the same
`prove_weld`, so a refusal it inherits is only ever a *hint* that closing had nothing to offer.
`Interior` and `Failed` deliberately do NOT fall through. Gated by
`the_l_notch_routes_to_extend_through_the_dispatcher`.

**Pinch vertices are handled by a sector-agnostic scan, not `boundary_edges_at`.** Per §5b finding
3 that helper answers for one sector only and cannot see a pinch. P4 uses a local `rim_edges_at`
collecting EVERY twin-less half-edge at the vertex: `(0,0)` → interior; `>1` on either side →
explicit `Pinch` decline ("two borders meet at that vertex — pick a vertex on just one of them");
one side only → `OneSided`; `(1,1)` → proceed. A useful side effect: it also rescues a `ChainEnd`
that was caused by a derailed sector walk, which is the verdict the owner's toast actually reported.

### Known limits of `X = A + E − V`, stated so they are not mistaken for bugs

* **A straight border cannot be grown this way.** With `A`, `V`, `E` collinear the rule yields
  `X == V` — a zero-area quad. P4 declines (`Straight`). Growing off a straight rim needs an EDGE
  EXTRUDE (two new vertices), which is a different operation, not this rule. **New spec item if
  wanted.**
* **A plain sheet's convex corner never reaches extend** — closing succeeds there first, laying a
  quad over the surface with the outer-rim warning, and "closing mode is UNCHANGED" was a hard P4
  requirement. Routing that case to extend means changing closing's over-surface behaviour.
  **Owner decision, likely P3 territory.** (This is NOT the owner's repro, which is the L-notch
  above.)
* **On a quad grid's convex corner `A + E − V` lands exactly on an existing diagonal vertex.** P4
  refuses (`CornerExists`) rather than creating a coincident vertex and a crack. The check is
  bounded to two rim steps each way, never a mesh scan.

### 2.3 The extrapolated corner needs a stated rule

For the extend case, `X = A + E − V` (the parallelogram completion — F2's own rule), then
**projected onto the plane of the face adjacent to `V`** so the new quad continues the surface
instead of leaving it. State this in the doc comment; a bare parallelogram in 3D drifts off any
curved border.

**The mouse does less work in extend mode**, because both boundary neighbours are consumed. It only
disambiguates when `V` has more than two boundary edges (a pinch point). Say so rather than
implying uniform behaviour.

### 2.4 "Repeatable" means the selection must advance

After a successful fill, move the single-vertex selection to the **next boundary vertex in the
walked direction**. Without that, a second `F` refills the same place (or declines), and the zip
workflow the owner chose does not exist. The advance is part of the feature, not polish.

---

## 3. Phases

### P1 — `boundary_edges_at` + the walk *(core_math, pure)*

Helper in `nav.rs` (§2.1) plus a boundary walk that, from a vertex and a starting boundary
half-edge, yields the next k boundary vertices.

**Gate (`cargo test -p core_math`):** on a grid with one quad deleted, `boundary_edges_at` returns
exactly two edges for each of the 4 hole-corner vertices and `None`/`None` for an interior vertex;
the walk from any hole corner returns the other 3 corners in boundary order, in both directions.

### P2 — Closing mode

Classification (§2.2) + fill via the existing `create_face`.

**CORRECTED 2026-08-09 — the original instruction here ("reuse `try_fill_ring`'s camera-facing
winding") was WRONG and would have shipped a bug.** Winding for this op is **topological, not
view-dependent**. `create_face_no_normals` twins each new half-edge `src→dst` by looking up an
existing `dst→src` (`euler.rs:510`), and a rim half-edge already runs in its adjacent face's
direction — so the filling face MUST wind *opposite* the rim or the twin lookup finds nothing.
Camera-facing winding is a coin flip: half the time it emits a face that duplicates every directed
edge and welds to nothing. This is the same rule `cap_boundary_loops` (`ops_cap.rs:58`) already
states. The gate must assert **the weld** — all rim half-edges lose their `NO_TWIN` — not merely
that a face appeared.

**Gate:** the owner's repro — a 4-corner gap, one vertex selected, `F` → one quad, correct winding,
`validate()` green. A 6-vertex hole fills the quad **nearest the selected vertex** and leaves the
rest open. An interior (non-boundary) vertex declines with a toast and mutates nothing (assert the
mesh is unchanged, per §1).

### P3 — Mouse direction + selection advance

Project both candidate directions to screen space (use `self.cam_ray` / the existing
`world_to_screen` path — and remember prim/vertex positions are body-local, so apply the body
transform first per CLAUDE.md § *Prim Position Space*), pick the one nearer the cursor. Then §2.4's
advance.

**Gate:** with the cursor on either side of the selected vertex, `F` fills the quad on that side —
tested by driving the app path with two cursor positions, not by unit-testing the picker alone
(`feedback_verify_app_path_not_kernel`). Tapping `F` four times around a 6-vertex hole closes it
without moving the mouse.

### P4 — Extend mode

`X = A + E − V` projected onto the adjacent face plane (§2.3); new vertex + new quad.

**Gate:** on an open border corner, `F` adds exactly one vertex and one quad; the new quad is
coplanar with the adjacent face to within a fixed tolerance; `validate()` and `is_manifold()` both
pass; and on a *curved* border the projection actually moves `X` off the raw parallelogram point
(non-vacuity — otherwise the projection is untested).

### P5 — Sidecar + menu

* CustomData: the new face's corners need UV/seam/sharp handling consistent with the neighbours it
  is welded to (`project_smart_select_and_sidecar` class). If it cannot propagate yet, declare it
  loudly rather than dropping silently.
* Add to the Vertex-mode menu next to Fill / Grid Fill with text that distinguishes the three.

**Gate:** a body with authored UVs keeps them across the op; the menu lists Fill, Quad Fill and
Grid Fill with non-overlapping descriptions.

---

## 4. FEATURE_RULES pre-flight

1. **New scene object type?** No. No `NodePayload` variant, no save-format change.
2. **Mutates scene state?** Yes — one atomic `transaction("Quad Fill", …)` per press (not an
   episode; this is a one-shot, not a drag). Undo gate: fill → `Ctrl+Z` → face gone, and in extend
   mode the invented vertex gone too.
3. **Frame-thread work?** No async, no poll loop, no cap needed.
4. **New render path or mode?** No.

`half_edge.rs` red line respected: the new helper is a read-only accessor; no serialized field is
retyped, reordered or removed.

---

## 5. Regression checklist

```bash
cd core_engine && cargo build -p app
cd core_engine && cargo test -p core_math
```

Manual:

1. 4-corner gap, one vertex selected, `F` → quad, winding matches neighbours.
2. 6-vertex hole → four taps of `F` zip it closed, all quads, no n-gon.
3. Cursor left vs cursor right → fills the corresponding side.
4. Open border corner → `F` grows a quad with one new vertex, on-surface.
5. Interior vertex → declines, and `Ctrl+Z` still undoes the *previous* action (the decline ate no
   undo entry).
6. `Ctrl+Z` after a fill removes face and any invented vertex in one step.
7. Existing paths unchanged: 3+ verts still fills; an edge loop still fills; `Ctrl+Alt+F` Grid Fill
   still grid-fills.
8. Save → reload → geometry identical.

---

## 5b. Findings from the P1/P2 implementation (2026-08-09) — read before P3/P4

### Outer rims — RESOLVED 2026-08-10 (owner decision: **fill, but warn**)

**A hole rim and an OUTER rim are topologically identical.** Nothing in the half-edge structure
distinguishes "the border of a hole in the middle of a sheet" from "the outside edge of the sheet".
So on a curved open sheet, selecting an outer-rim vertex closes a quad *over* the surface. Only the
flat case is caught by geometry alone (collinear → decline).

**Owner decision: the fill PROCEEDS and a toast reports what happened.** Not a refusal.
Rationale: it is honest, undoable in one `Ctrl+Z`, and it never blocks a legitimate fill.

The detector is the fold test — the new face's centroid must lie on the FAR side of each shared
edge from that edge's existing neighbour face; on the near side it folds back over the surface.
**It is deliberately a WARNING and not a gate**, because the test false-negatives on concave rims
and a hard refusal would lock users out of valid work. Do not "upgrade" it to a refusal later
without a new owner decision — that regression is the reason this paragraph exists.

### Pre-existing kernel weaknesses found (NOT introduced here, NOT fixed here)

1. **`create_face` cannot refuse a bad ring.** `create_face_no_normals` (`euler.rs:478`) returns
   `None` **only** when `vertex_ids.len() < 3`; after that it unconditionally allocates and returns
   `Some`. The twin pass (`euler.rs:507-517`) is best-effort — a failed `find_half_edge` silently
   leaves `twin == NO_TWIN`. Therefore the long-standing "dry-run `create_face` on a clone" in
   `try_fill_ring` proves nothing beyond the length check. P2 compensates by additionally running
   `validate()` and `is_manifold()` on the clone (the latter only held against us when the original
   was already clean, so an already-non-manifold body stays editable).
   **FIXED 2026-08-10 (owner decision: "fix now").** `prove_weld()` is now the single
   implementation of "would this face actually weld?", shared by both the 3+-vertex and
   single-vertex paths. Each check is held against the result **only when the ORIGINAL passed it**
   (extended to `validate()` as well as `is_manifold()`), so an already-broken body stays
   repairable — the same principle behind `integrity()` being split out of `is_manifold()` for edit
   gates (`validate.rs:150`, owner directive 2026-08-03: "all tools must work even on open /
   non-manifold shapes").

   **The guard alone would have been a REGRESSION — the reversed retry is what makes it safe.**
   `is_manifold`'s check (a) ("directed edge used by 2 faces, same winding") fires on exactly the
   ring the camera-facing flip produces from the wrong side of a hole. Adding the guard by itself
   would therefore have converted roughly half of all camera angles from "silently makes a broken
   face" to "refuses to fill". Because winding here is **topological, not view-dependent** (§4-P2's
   correction), the fix is to retry the reversed ring: those cases become CORRECT fills instead of
   refusals. Order matters and is deliberate — camera winding is tried FIRST, so a free-floating
   ring (where both windings weld and only the normal direction is at stake) keeps its previous
   camera-facing behaviour. A `Failed` ring is never retried: too short stays too short.

   Remaining limit, stated in code rather than hidden: on an already-non-manifold body the manifold
   gate is disabled by design, so the retry cannot fire there and the camera choice stands. That is
   byte-for-byte the pre-guard behaviour, but a dirty body can still take a badly-wound fill.
   Closing it needs a signal that survives a dirty mesh (per-winding welded-rim-edge count).

2. **`delete_face` can leave a hole corner's disk pointer aimed at the rim half-edge**, so
   `vertex_fan` there yields a single edge and never covers the sector. Since `find_half_edge` fans,
   it can miss real edges at boundary vertices — which makes `create_face`'s twinning unreliable
   exactly where this feature operates. The new helpers dodge it by seeding and walking backward.
   Combined with (1), this means the existing Fill can silently produce a face welded to nothing.

3. **`boundary_edges_at` answers for one sector only** — the sector `vertex.half_edge` names. A
   pinch vertex has several boundary pairs. P3's "more than two boundary edges" case (§2.3) will
   need a multi-pair API; the single-pair limit is documented on the function.

---

## 6. Session log

* **2026-08-09** — P1+P2 implemented. `boundary_edges_at` / `boundary_walk_from` /
  `boundary_closing_ring` appended to `nav.rs` (append-only; no existing function touched, because
  vertex-bevel work was live in that file concurrently) + 12 tests. Closing mode added to
  `fill.rs` behind a `verts.len() == 1` branch; all existing Fill paths byte-identical. Six decline
  paths, all returning before `transaction()` so a refusal never eats Ctrl+Z. Extend mode toasts
  only (P4). `cargo build -p app` green; `cargo test -p core_math` 1066 passed / 3 failed = exactly
  the known pre-existing set, no new failures. Two plan errors corrected in place (§2.2
  self-contradiction, §4-P2 camera-facing winding) and findings recorded in §5b.
* **2026-08-09** — Plan authored from owner reference image + three settled decisions (§0).
  Verified: the single-vertex case dead-ends at `fill.rs:42`; `F` is already `ACT_POLY_FILL` so no
  new binding is needed; reading `viewport_mouse` from a key handler is precedented
  (`poly_start_inset_drag`); `vertex_fan` breaks at boundaries and therefore cannot supply both
  boundary edges (§2.1) — that is the first real implementation trap. No code changed.
