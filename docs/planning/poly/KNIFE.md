# KNIFE.md — Poly Knife Knowledge Base (Read First)

Accumulating knowledge for the polygon **knife** (`core_math/src/knife.rs`, app glue `app/src/poly_knife.rs`).
Same discipline as `BEVEL.md`: this file exists so we never re-derive a dead-end. **When you finish a knife
task, APPEND**: any new dead-end → §5, a one-line §7 session-log entry, update the bug tracker.

---

## 1. The Model (reverse-engineered from Blender's `editmesh_knife`)

The knife is a **screen-space polyline cut**. The user places points; each point resolves to a cut node and
the segments between them cross mesh edges. Blender's knife builds a network of KnifeVerts/KnifeEdges and
re-tessellates each touched face. We do the equivalent with the **half-edge `from_polygons` rebuild pattern**
(the sanctioned poly pattern, same as `bevel_op`).

**A clicked point is one of four kinds** (classified against the mesh in mesh-LOCAL 3D):
- **on a vertex** → snaps to it (a Bound node; no new geometry).
- **on an edge** → the edge is split → a Bound node.
- **inside a face** → a real interior cut vertex (Interior node).
- **off the surface** (behind / outside every visible face) → a **Break** node: it segments the path so two
  crossings on either side are NOT connected (the cut left the surface — no spurious cut).

Between consecutive clicked points, the straight screen segment **crosses edges**; each crossing is also a
Bound node (the edge is split there). An edge may be crossed **multiple times** (a U-turn / sawtooth) — each
crossing becomes its own vertex (progressive split).

**Connecting nodes → the cut:**
- Two Bound anchors with an interior run between them → **through-cut**: the face is split into two along the
  exact chain `a → interior… → b` (boundary arc + interior chain). Divides the face (+1 face). Handles a
  U-turn (a,b adjacent on the ring).
- An interior run **before the first / after the last** Bound (dangling), or a chain returning to the same
  vertex → **slit**: a spur ring `[.. a, i0..ik, i_{k-1}..i0, a ..]`. Adds the cut edges but does **not**
  divide the face (Blender does the same — a dangling cut is a visible spur, zero-width until pulled).

**This matches Blender:** a cut that reaches an edge/vertex on both ends of a face **divides** it; a cut that
ends inside a face leaves a **dangling spur**. Interior→edge→interior (one boundary anchor) = two slits, one
per incident face — it does NOT divide (correct).

## 2. Pipeline (`knife_cut_screen`)

1. **Crossings** — for every path segment × every primary edge, screen segment/segment intersection → the
   crossing edge + screen param. Read-only.
2. **Classify clicks** — vertex / edge / interior / off (Break), using mesh-local `poly_3d`.
3. **Materialise edges** — group crossings by edge, split each edge PROGRESSIVELY at all its crossings
   (ascending edge-order, local param `(s3-base)/(1-base)`, merge near-coincident). `split_edge` is safe
   (preserves twins).
4. **Node list** — Bound / Interior / Break in path order; dedup consecutive equal Bounds, collapse Breaks.
5. **Polygon-soup split** — snapshot the edge-split mesh as index-list polygons; walk runs between Breaks;
   through-cut / slit each face by **pure index math** (`split_poly_through` / `split_poly_slit`).
6. **Rebuild ONCE** via `from_polygons` (global twin re-pairing).

## 3. Invariants (do not break)

- **Never incremental `delete_face`/`create_face` for the face splits.** It cannot compose across multiple
  splits in one stroke (see §5). Always snapshot → index-split → `from_polygons`.
- **`split_edge` for crossings only** — it is the one incremental op that is twin-safe.
- **Face routing uses PLANE distance, not just the footprint** (`plane_dist` + `poly_contains`). A shared-edge
  anchor lies on two faces; route each cut to the face whose PLANE the cut point is on. Footprint alone gives
  a cross-plane false positive (see §5).
- **Off-surface clicks are Breaks**, not dropped — otherwise an off-mesh dip draws a spurious cut.
- **App safety net** (`poly_knife_confirm` / the slice dry-run) uses **`knife_result_is_sound()`**
  (`app/src/poly_knife.rs`), NOT `is_manifold()`. *(Tightened 2026-08-07 — read the reasoning before
  "upgrading" it.)* `validate()` alone was too weak (it passed the owner's pinched circle ring), but a blanket
  `is_manifold()` is too STRONG: `integrity()` rejects ANY repeated vertex in a face, and a dangling slit
  revisits its anchor through distinct spur edges BY DESIGN (§1) — `is_manifold()` would refuse every
  legitimate slit, the exact class this file exists to protect. The chosen predicate is: `validate()` **+** no
  vertex appearing **3+ times** on one ring (a multi-fold self-touch; a legal spur touches its anchor exactly
  twice) **+** no zero-length (adjacent-repeat) edge **+** edge multiplicity (a directed edge used by >1 face,
  or an undirected edge incident to >2 faces — the twin-corruption class `is_manifold()` was wanted for, and
  one that slits do not trip).
  **Known limit, deliberate:** a *two-occurrence* spur ring is topologically indistinguishable from a legal
  slit, so the gate could NOT have rejected the owner's circle. "A CLOSED loop must DIVIDE" is a kernel
  property, enforced by `split_poly_closed_loop` (§5.5) and locked by tests — not something a post-hoc
  soundness check can see. Do not try to encode intent in the gate.
- **Nodes chain by FACE CONNECTIVITY, never by raw path order.** With `through=true`, hidden-edge crossings
  land in the same path-ordered stream as the visible surface chain (screen order interleaves depth sheets).
  The run walk groups nodes into face-connected chains (a node joins the first chain whose last node shares a
  soup face with it) and connects each chain independently — a hidden crossing must never steal a slit/through
  anchor from the visible chain.
- **`purge_degenerate_faces` must NOT treat a non-adjacent repeated vertex as degenerate.** A slit ring
  revisits its anchor through distinct spur edges BY DESIGN; purge only rings with <3 unique corners or a
  zero-length (adjacent-repeat) edge. The old any-repeat rule silently deleted every slit-carrying face.
- **`split_poly_slit` face binding is plane-GATED** (not merely plane-ranked): candidates whose plane the
  probe is off by >2% of the face diameter are rejected outright — winding containment alone reads the whole
  infinite prism as "inside" (§5.4's trap, one level up).
- **A CLOSED path's node stream is CYCLIC.** Before the run walk, rotate `nodes` left past the last Break so
  no run straddles the wrap, then re-dedup. Without it a closed cut whose path leaves the surface (any rect /
  circle notch open through a silhouette edge) splits its one through-cut into two dangling slits (§5.4).
- **Ctrl+Z while the knife has points pops the last point** (tool-local undo), never the scene.
- **Confirm commits the live hover snap** (the red-cursor point) before cutting.
- **Ctrl is H/V snap, never "disable snap".** Same as Draw Polyline. Shift / magnet + snap°
  is angle increment. A 2026-08-03 inversion that made Ctrl a free-surface-only early-out
  is a dead-end (§5.8) — do not restore it.

## 4. Tests (regression coverage)

- Unit (`knife.rs` mod tests): straight/diagonal/miss/closed, n-gon slit, interior zigzag, U-cut, multiface.
- Integration (`core_math/tests/`): `knife_stress_{bevelwrap,multicross,dangling,closedloop,thruvert}` (the
  5 adversarial families) + `knife_matrix` (divide vs slit + the cross-plane-merge regression).

## 5. Graveyard (proven dead-ends — do NOT retry)

1. **`split_face_between(a,b)` + reposition split verts** for interior runs — straightens then bends → tangles
   non-monotonic runs into a self-intersecting n-gon; rejects adjacent a,b (U-turn) dropping the whole run.
2. **One split per edge** — drops the 2nd crossing of a U-turn/sawtooth → run degrades to a slit → the ear-clip
   fans a self-intersecting n-gon (the "triangle-fan spray").
3. **Incremental `delete_face` + `create_face` for the splits** — `create_face` re-pairs twins via a
   boundary-terminating vertex fan; when one stroke splits a face then re-cuts a CHILD (sawtooth notches,
   U-turn over a fold edge shared by two cut faces), a split vertex has >1 incident boundary half-edge and the
   fan mis-links → non-manifold (`twin.twin != self`). **Root fix = polygon-soup + `from_polygons`.**
4. **Treating a CLOSED path's node stream as LINEAR** — the run walk (`while ci < pe.len()`) scans a flat
   array, so on a closed path any run that WRAPS around index 0 is split in two. A rect slice whose top side
   leaves the silhouette classifies as `I… B, BRK, B …I` (the off-surface side is a Break BETWEEN the two
   crossings), so each half held a single anchor and degraded the through-cut into TWO DANGLING SLITS — and
   each half re-materialised the closed polyline's first==last corner as its own vertex (coincident duplicate
   pair). **Fix = rotate `nodes` left past the LAST Break when `closed`, then re-dedup consecutive nodes**
   (the rotation is what makes the duplicated terminal pair adjacent, so only then can the dedup see it).
5. **`PE::Interior` allocating a fresh position per node with no global dedup** is only safe because the
   stream is de-duplicated first. Any change that lets two nodes at the same point survive into `pe`
   re-creates the coincident-duplicate class — dedup at the NODE level, never by post-merging positions.
6. **Footprint-only face routing** (angle-sum `poly_contains` with no plane check) — a front-face cut point sits
   under the top face's footprint → binds to the wrong face → the two faces merge into a cross-plane tangle
   (the "cut that won't work"). **Fix = `plane_dist` min-selection.**

5. **Treating a CLOSED interior loop as "just another anchor-less chain"** — the current
   `knife_cut_screen` anchor-less branch (`knife.rs:722-739`). A circle drawn wholly inside one
   face crosses no edge and snaps to no vertex → no Bound anchors; its head and tail are the SAME
   point, so `nearest(head) == nearest(tail)` and the `(Some(a), _)` fallback emits
   `split_poly_slit` — a spur that does **not** divide the face. This is the owner's 2026-08-07
   "KNIFE CIRCLE FAIL TO DO, GOT SEGMENT AFTER CUT": the circle appears as edges, the face is
   never split. `closed` is a parameter of `knife_cut_screen` and the anchor-less branch never
   reads it. **A closed interior loop needs its OWN case** (§5.5 below).
7. **A depth phase whose value only moves once a button is pressed reads as a DEAD KEY.** The first
   cut of the `E` depth phase (§6.1) seeded a reference on LMB-press and integrated a delta from
   there — arithmetically correct, and the create tool's extrude phase does the same. But in the
   knife the phase is entered MID-DRAG, so the user presses `E`, releases the sizing button, moves
   the mouse — and nothing moves, because no press has happened since. Owner verdict: *"E don't
   work"* + *"the rectangular UI stays behind after I release left click"* (i.e. the locked shape
   looked like a stuck overlay, not a live phase). **Fix = track the pointer ABSOLUTELY on hover
   from the moment the phase arms** (`poly_knife_shape_depth_track`), and RECOLOUR the locked
   overlay so "armed" is visible before the mouse moves at all. Generalises: any modal phase entered
   while a button is already down must show life without requiring a fresh press.

8. **`Ctrl = no snap` (free-surface early-out)** — `096ed32b` / `poly_knife_hover_inner`.
   The HUD, snap bar, and Draw Polyline all advertise **Ctrl = H/V / snap**. Treating Ctrl
   as "disable all snapping" made the advertised snap a dead key: hold Ctrl over a vertex
   and the green square vanished (kind 0 mid-face). Owner 2026-08-17: *"the snapping
   function do not come"*. **Fix = delete the early-out.** Ctrl builds the same screen-space
   constrain ray as angle-snap, but H/V; vertex/edge snap stay on and filter to that ray.
   First point (no previous) keeps always-on vert/edge snap even with Ctrl held.

6. **Do NOT drop the repeated closing point in `poly_knife_shape_rebuild`**
   (`app/src/poly_knife.rs:1209`) to kill the coincident duplicate vertex.
   `knife_free_is_closed()` decides closure from first↔last distance — remove the repeat and the
   circle reads as an OPEN line and takes the SUBTRACT path instead of the slice. The duplicate
   must be dropped inside the kernel's closed-loop case.

### 5.5 The closed-interior-loop fix — **LANDED 2026-08-07** (`knife.rs`)

**Status: implemented**, exactly as designed below, with ONE deviation worth knowing:
the `is_loop` guard accepts BOTH `gap ≈ 0` (the duplicated closing sample is present → `lp.pop()`)
and `gap ≈ one step` (a node-level dedup already ate it → keep every point). The original text only
handled the first; the second is the case a 4-corner closed loop inside one grid cell takes, and
without it that case still fell through to the anchored chord. Everything else is as written.

Verified before/after through the REAL app path
(`app/src/poly_knife.rs::s_circle_inside_one_face_divides_it_and_never_pinches`): with the branch
disabled the cut leaves two rings that walk the circle out and back
(`[25..41, 40..25, 3,2,5,4,3]`) — the owner's exact signature; with it enabled, 0 pinched rings,
0 boundary edges, and the front sheet becomes 3 faces (disc + two annulus halves).



**Blender oracle** (`docs/planning/poly/circle_knife_oracle.py`, cylinder r=0.35 DIFFERENCE box
±1, solver `EXACT`): `nv=40 nf=24 ne=64`, non-manifold 0, boundary 0, wire 0, orphan 0,
coincident 0, **pinched 0**, arity `{4:20, 7:2, 17:2}`. Blender splits the resulting ANNULUS into
**two SIMPLE polygons** (7 + 17 = 4 ring corners + 16 loop verts + 4 bridge duplicates). So the
target is *inner disc + two annulus halves* — **never a keyhole ring with a repeated vertex, and
never a spur**. Legality proven in our kernel by `core_math/tests/knife_circle_annulus.rs`.

**(a) New helper**, beside `split_poly_slit` (~`knife.rs:960`):

```rust
/// Split the face carrying a CLOSED interior loop into inner disc + two annulus halves
/// (Blender-EXACT parity — `circle_knife_oracle.py`: pinched_faces = 0, annulus arrives as
/// two SIMPLE polygons). Never a keyhole ring, never a slit spur: a spur does not divide the
/// face, which is the owner's "GOT SEGMENT AFTER CUT" (§5.5).
/// `lp` = the loop's position indices in traversal order, WITHOUT a repeated closing sample.
/// Fails closed (false) if there is no carrier face, if the loop is not wholly inside it, or
/// if two non-crossing bridges cannot be found.
fn split_poly_closed_loop(
    polys: &mut Vec<Vec<usize>>,
    mats: &mut Vec<u32>,
    positions: &[Vec3],
    lp: &[usize],
) -> bool {
    if lp.len() < 3 { return false; }
    // Carrier: contains EVERY loop point AND is plane-GATED (>2% of face diameter = reject).
    // Winding containment alone reads the whole infinite prism as inside (§3).
    let probe = positions[lp[0]];
    let mut best: Option<(usize, f32)> = None;
    for (i, poly) in polys.iter().enumerate() {
        if poly.len() < 3 { continue; }
        if !lp.iter().all(|&x| poly_contains(positions, poly, positions[x])) { continue; }
        let mut diag = 0.0f32;
        for k in 1..poly.len() {
            diag = diag.max((positions[poly[k]] - positions[poly[0]]).length());
        }
        let d = plane_dist(positions, poly, probe);
        if d > diag.max(1e-6) * 0.02 { continue; }
        if best.map_or(true, |(_, bd)| d < bd) { best = Some((i, d)); }
    }
    let pi = match best { Some((i, _)) => i, None => return false };
    let ring = polys[pi].clone();
    let (n, k) = (ring.len(), lp.len());

    // Orient the loop to MATCH the parent ring, so the disc keeps the parent's normal and
    // each annulus half walks the loop backwards (= a hole).
    let newell = |ids: &[usize]| -> Vec3 {
        let mut s = Vec3::ZERO;
        for i in 0..ids.len() {
            s += positions[ids[i]].cross(positions[ids[(i + 1) % ids.len()]]);
        }
        s
    };
    let mut lp: Vec<usize> = lp.to_vec();
    if newell(&lp).dot(newell(&ring)) < 0.0 { lp.reverse(); }

    // Two bridges that cannot cross: the nearest visible (ring, loop) pair, then the pair
    // closest to diametrically opposite on BOTH cycles. Midpoint containment is the same
    // visibility filter the anchor-less chord uses on a concave ring (fail closed).
    let visible = |ri: usize, li: usize, lp: &[usize]| -> bool {
        poly_contains(positions, &ring, (positions[ring[ri]] + positions[lp[li]]) * 0.5)
    };
    let d2 = |ri: usize, li: usize, lp: &[usize]| {
        (positions[ring[ri]] - positions[lp[li]]).length_squared()
    };
    let mut b0: Option<(usize, usize)> = None;
    for ri in 0..n { for li in 0..k {
        if !visible(ri, li, &lp) { continue; }
        if b0.map_or(true, |(r, l)| d2(ri, li, &lp) < d2(r, l, &lp)) { b0 = Some((ri, li)); }
    }}
    let (r0, l0) = match b0 { Some(x) => x, None => return false };
    let opp = |ri: usize, li: usize| -> f32 {
        let ra = (ri + n - r0) % n;
        let la = (li + k - l0) % k;
        (ra as f32 / n as f32 - 0.5).abs() + (la as f32 / k as f32 - 0.5).abs()
    };
    let mut b1: Option<(usize, usize)> = None;
    for ri in 0..n { for li in 0..k {
        if ri == r0 || li == l0 { continue; }
        if !visible(ri, li, &lp) { continue; }
        if b1.map_or(true, |(pr, pl)| opp(ri, li) < opp(pr, pl)) { b1 = Some((ri, li)); }
    }}
    let (r1, l1) = match b1 { Some(x) => x, None => return false };

    let arc = |cyc: &[usize], from: usize, to: usize| -> Vec<usize> {
        let m = cyc.len();
        let (mut out, mut i) = (Vec::new(), from);
        loop { out.push(cyc[i]); if i == to { break; } i = (i + 1) % m; }
        out
    };
    // Half A: ring arc r0→r1, bridge, loop BACKWARDS l1→l0, bridge home.
    let mut half_a = arc(&ring, r0, r1);
    let mut i = l1;
    loop { half_a.push(lp[i]); if i == l0 { break; } i = (i + k - 1) % k; }
    // Half B: ring arc r1→r0, bridge, loop BACKWARDS l0→l1, bridge home.
    let mut half_b = arc(&ring, r1, r0);
    let mut i = l0;
    loop { half_b.push(lp[i]); if i == l1 { break; } i = (i + k - 1) % k; }
    if half_a.len() < 3 || half_b.len() < 3 { return false; }

    let mat = mats.get(pi).copied().unwrap_or(0);
    polys[pi] = half_a;
    polys.push(half_b); mats.push(mat);
    polys.push(lp);     mats.push(mat); // the inner disc
    true
}
```

**(b) Call site** — inside the anchor-less branch, immediately after the
`if inter.iter().all(|&p| poly_contains(&positions, &polys[pi], positions[p])) {` line
(`knife.rs:706`), BEFORE `let head = positions[inter[0]];`:

```rust
                        // CLOSED interior loop (the C-shape knife drawn wholly inside one
                        // face) divides into disc + annulus — never a slit spur. Without
                        // this, both free ends resolve to the SAME nearest ring vertex and
                        // the `(Some(a), _)` arm below emits the spur the owner reported as
                        // "GOT SEGMENT AFTER CUT" (§5.5).
                        let is_loop = closed
                            && inter.len() >= 4
                            && (positions[inter[0]] - positions[*inter.last().unwrap()]).length()
                                <= (positions[inter[0]] - positions[inter[1]]).length() * 0.5;
                        if is_loop {
                            let mut lp = inter.clone();
                            lp.pop(); // the duplicated closing sample (see §5.6)
                            if split_poly_closed_loop(&mut polys, &mut poly_mat, &positions, &lp) {
                                if knife_diag {
                                    eprintln!("[KNIFE-DIAG] closed interior loop k={} -> disc + 2 annulus halves", lp.len());
                                }
                                cut_edges += 1;
                                continue; // `for chain in &chains`
                            }
                        }
```

`closed`, `poly_mat` and `knife_diag` are all already in scope at that point.

## 6. Open / Next

- Dangling slits are zero-width in SOLID shading (invisible until pulled) — correct topology, but consider a
  visible feedback for edit-mode. (Not a bug; Blender behaves the same.)
- Pending features: bevel-along-polyline (drag=width, scroll=segments), Ctrl view-based polylasso mode, X pie.
- Free-slice cosmetic residue: 1-2 sub-visible densify kink verts (~0.5% chord error) can remain on the cut
  edges (`+6v` where ideal is `+4v` on a box). Proper fix would straighten layer interior points onto the
  exact face∩cut-plane line before the cut; not worth it until it shows visually.

## 6.1 Shape cut DEPTH (`E`) — LANDED 2026-08-09

Owner: *"knife tool shape project (rec, cir) while dragging pressing E should let you define the
thickness of the cut, user needs to be able to rotate camera, cut confirms when user releases the
left click."* Gesture model chosen by the owner from three offered: **E locks the shape, the second
drag is the depth.**

| Step | What happens |
|---|---|
| LMB press-drag | sizes the rect / circle as before |
| **`E`** | LOCKS the silhouette (overlay turns ORANGE), enters the depth phase |
| LMB release | ends **sizing only** — deliberately does NOT cut |
| **move the mouse** | the cut extrudes **to the pointer** — absolute, hover, no button held |
| MMB orbit / wheel zoom | free: this is why the phase exists (you cannot judge depth from the face you drew on) |
| LMB release | **commits the cut** (a press must have landed in the phase first) |
| Esc / tool switch | cancels, depth reset |

- **Depth = a bounded cutter prism, nothing else.** `poly_knife_free_boolean` spans `[-depth,
  +big*1e-3]` along `n` instead of `±4×` scene bound. Everything downstream (Manifold difference,
  `result_installable`, undo) is unchanged, which is why a pocket cannot diverge from a through cut.
- **Zero depth never becomes a through-hole by accident.** `knife_cut_depth()` treats depth ≤ 1e-4 ×
  scene bound as "no depth" (a zero-height prism is a degenerate cutter the boolean rejects), so
  every OTHER verb — Enter, `S`, an open mesh — falls back to the through cut it always did. But a
  depth-phase RELEASE with no drag does **not** commit: it toasts and stays in the phase. Committing
  there would punch a full through-hole the user never asked for, and the destructive reading of an
  ambiguous gesture is the wrong default.
- **One commit path.** `poly_knife_shape_commit_cut()` — plain release, depth release and a
  depth-armed Enter all call it. Do NOT fork it per gesture (that defect is what the 2026-08-08
  rect-corner-bevel fix removed; see §7).
- **Depth is subtract-only, and says so.** A slice SCRIBES the surface and an open mesh has no
  thickness, so both toast and cut through instead of silently dropping the depth or silently
  switching the user's verb.
- **Depth is ABSOLUTE — the floor sits where the pointer is** (`poly_knife_shape_depth_track`, run
  every frame of the phase, hover included). Axis parameter from `compute_extrude_depth`
  (`ops/geo/placement_math.rs`, shared with the create tool's extrude phase). MMB suspends tracking
  for the duration of the orbit gesture only — during an orbit the pointer sweeps the screen and the
  depth would swing with it.
- **The camera must stay fully usable in the phase**: `knife_shape_scroll_armed()` returns false so
  the wheel zooms instead of changing segments on a shape that is already locked.
- **The extrusion preview is the feature's only proof of life** — the silhouette is frozen by
  design, so without the swept solid the phase looks like a stuck rectangle (owner: *"i need to see
  the shape extrude, otherwise user think nothing append"*). It draws the ring swept along −n as
  shaded quads + floor ring + ribs + the depth readout on the floor.
  ⚠ **It MUST live in `draw_knife_shape_select_overlay` (`draw_poly_wire.rs`), not
  `draw_knife_overlay`** — the latter early-returns to a HUD line whenever a shape is anchored,
  i.e. for the entire depth phase, so a preview drawn there never reaches the screen (cost: one
  round of "E does nothing"). It is also the ungated path, so the preview survives "poly overlays
  off" exactly like the outline it belongs to.
  ⚠ Pair the two rings from `knife_points` directly. `screen_poly` in that function is a
  `filter_map` that DROPS unprojectable points, so its indices do not line up with `knife_points`
  and sweeping rim[i]→floor[i] off it joins the wrong corners as soon as one point leaves the view.
- Gate: `rect_depth_cut_makes_a_blind_pocket_and_leaves_the_far_face_whole` (app path). Its CONTROL
  half — same points, depth off — is load-bearing: without it every pocket assertion would also pass
  on a tool that cut nothing, and it pins the through cut as untouched by this feature.

## 6.2 Boolean TYPE (`U` / `I`) + signed depth — LANDED 2026-08-11

Owner: *"with shape knife tool pressing [I] should let you change the boolean type to intersection,
U to union. On the E extrude I can only go one direction, need to be able to go in and out for the
union (watch out for perfect plane alignment on the union, when drawing the shape on the surface)."*

- **One cutter, three operators.** `knife_bool_op: KnifeBoolOp {Subtract, Union, Intersect}` picks
  the `BoolOp` handed to `mesh_boolean` in `poly_knife_free_boolean`. Nothing else about the cutter
  changes — the same drawn outline can remove, add or keep material without redrawing it.
  `U` / `I` (`ACT_POLY_KNIFE_BOOL_UNION` / `_INTERSECT`) toggle: the same chord again returns to
  Subtract, so three ops need two chords and there is always a way back without leaving the tool.
  `I` shares its chord with poly Inset by STATE, consumed first in `poly_handle_keys` — the same
  pattern `E`/Extrude and `P`/Separate already use. The op is STICKY for the session (like
  `knife_free_slice`): a refused cut must not silently throw the user's choice away.
- **The depth is SIGNED.** `poly_knife_shape_depth_track` no longer clamps at 0, and
  `knife_cut_depth()` thresholds on `abs()`. `d > 0` reaches INTO the surface (pocket), `d < 0`
  stands OUT of it (the height of a Union boss). The old clamp made the outward half of the axis
  dead — the cursor moved and the number stayed 0 — which is why a Union could never be given a
  height. It was correct only while Subtract was the only op.
- ⚠ **THE COPLANAR TRAP (the owner's own warning, made structural).** A shape drawn ON the surface
  and extruded OUT gives the union a base cap *exactly* coincident with the face it stands on — two
  coplanar faces, the degeneracy the boolean fold class is worst at. So the outward prism spans
  `[-bite, +height]`, sinking its base `bite = big*1e-3` INTO the solid. The overlap costs nothing
  (that material is added anyway) and removes the coincidence entirely. `big*1e-3` is the same
  epsilon the inward pocket already used to keep its entry cap off the surface plane — do not drop
  it to zero "because the base should be flush". The height is also clamped to `> 2*bite` so the cap
  can never fall below the base (the ring order is what fixes the cap winding).
- **Three refusals, all fail CLOSED** (nothing committed, message names the key that fixes it):
  Union with no height (the unbounded prism would weld a rod 4x the scene bound through the model);
  Subtract pulled OUTWARD (it would shave a sliver — that is a pull in the wrong direction);
  Union/Intersect on an OPEN mesh or an OPEN outline (a surface has nothing to add to or keep, and
  the open-line cutter is a half-plane ribbon). A wrong op is worse than no op.
- **An explicit `U`/`I` outranks a face selection.** `poly_knife_shape_commit_cut` only routes to
  the slice when the op is Subtract: the user asked for something a scribe cannot express.
- Gates: `union_with_outward_depth_grows_a_boss_on_the_surface` (watertight, top at +0.4, nothing
  above the face outside the drawn outline — this is the coplanar case), `intersect_keeps_only_the_
  enclosed_column`, `union_without_a_height_refuses_instead_of_welding_a_rod`,
  `subtract_with_outward_depth_refuses`, `union_on_an_open_mesh_refuses`,
  `cut_depth_keeps_its_sign_for_an_outward_pull`, `bool_op_chords_toggle_back_to_subtract`.

## 6.3 Auto-select the cut result — LANDED 2026-08-11

Owner: *"knife cut should also auto set selection, set the resulting geo from boolean."*

- `knife_select_after_cut` flipped **ON by default** (it shipped OFF 2026-08-04); the toggle is now
  the opt-out, not the opt-in.
- After a BOOLEAN the selection is the geometry the boolean CREATED — cut walls, pocket floor, boss
  cap — via `knife_apply_boolean_result_selection` → Face mode. The rule is **every vertex of the
  face is new**; a face the boolean merely reshaped (the top face that gained a hole) keeps at least
  one pre-cut vertex and is correctly excluded. New-by-POSITION, never by id: both engines rebuild
  the mesh and recycle freed vertex slots (`knife_new_vertex_flags`, shared with the edge path).
- **Falls back to the rim edges** (the existing `knife_apply_cut_selection_filtered`, Edge mode) when
  the cut made no new face — the open-mesh cookie-cut REMOVES faces, so on a surface the rim IS the
  result. Scribe/slice verbs are unchanged: they still select the rim.
- Ordering: the selection runs INSIDE the commit transaction, after the mesh mutation and BEFORE the
  caller re-arms the knife — `poly_switch_work_mode` cancels an armed knife. Safe because the knife
  can only be armed from a poly EDIT mode (`poly_activate_knife` refuses in Object), so the switch is
  always edit→edit and `poly_auto_apply_before_component_edit` returns 0; an Object→edit switch there
  would run `transaction_snapshot_only` INSIDE the outer transaction and corrupt the undo stack.
- ⚠ **THE TRAP THIS SHIPPED WITH (owner, same day: *"you broke the click and drag cut feature with
  shape"*): A TOOL'S OUTPUT MUST NEVER READ BACK AS ITS INPUT.** Every knife verb reads a Face-mode
  selection as *"cut only these polys"*. The auto-select now WRITES one — so after a rect cut the
  tool sat in Face mode holding the cut walls, and the next rect drag scoped itself to them and
  scribed instead of punching a hole. The click-drag gesture still worked perfectly; the **verb**
  had silently changed. Fix: `knife_has_user_face_scope()` — ONE definition of "the user scoped
  this cut", used by all five read sites (shape commit, free confirm, classic confirm, slice
  commit, the free-line hint). It compares the live selection against `knife_last_result_faces`
  **by value**, not with a flag: the selection stops matching the moment the user touches it, so
  there is no invalidation contract for a later caller to forget — and the knife owns the pointer
  while armed, so a flag would have needed clearing from writers that cannot even run.
- Gates: `solid_subtract_auto_selects_the_boolean_result_faces` (asserted by PROPERTY — every
  selected face inside the cut footprint, all four walls hit — not by face count, which a legal
  retessellation would break), `open_mesh_subtract_auto_selects_the_cut_rim`,
  `select_after_cut_defaults_on`, and **`two_shape_cuts_in_a_row_both_punch_a_hole`** — the
  regression above; it asserts the FAR face gains a second rim, because face-count alone passes on
  a slice that cut nothing away.

## 7. Session Log

- 2026-08-17: **Ctrl snap restored** (owner: *"hold ctrl for snap using the knife, the snapping
  function do not come"*). `096ed32b` inverted Ctrl to "no snap" (free surface only), which
  contradicted the HUD (`Ctrl/magnet = Straight + angle`), the snap-bar tooltip, and Draw
  Polyline. Restored: **Ctrl = H/V lock** (vert/edge snap stays on, filtered to the ray);
  Shift/magnet = angle °. New graveyard §5.8. Gates: `ctrl_on_first_point_still_snaps_to_vertex`,
  `ctrl_locks_rubber_band_to_screen_h_or_v`, `ctrl_h_v_also_locks_the_free_polyline_plane`.

- 2026-08-11: **Boolean type `U`/`I` + signed depth** (§6.2) and **auto-select the cut result**
  (§6.3). 52 app knife tests green.

- 2026-08-09: **Shape cut DEPTH on `E`** (§6.1). New `ACT_POLY_KNIFE_DEPTH` shares `E` with poly
  Extrude, split by STATE and consumed first in `poly_handle_keys` — the same pattern
  `ACT_POLY_PIPE` uses for `P`. 43 app knife tests green.

- 2026-08-08 (b): **SUBTRACT NOW WORKS ON OPEN MESHES** (owner: *"can we make the boolean work on an open
  mesh, it would be useful also when you use the knife tool on a simple plane"*). It refused with "Knife
  subtract needs a closed mesh — this one has open edges (use Slice)".
  **A solid boolean on a surface is not a missing feature, it is undefined** — Manifold needs a watertight
  operand and a surface has no interior, so Difference has no answer. But the operation the user wants IS
  well defined: scribe the outline in, then drop the faces it encloses. The scribe is pure surface topology
  and **already worked on open meshes** (probe: 2×2 quad + closed rect → v=9/f=3, the enclosed rect is its
  own face) — only the solid path refused. So `poly_knife_free_boolean` now checks
  `boundary_half_edge_count > 0` and routes to `poly_knife_free_slice_commit` with the new
  `SliceEnclosed::Remove(rings)`, handing it **the very polygons the prism would have been extruded from** —
  open and closed meshes therefore cut the identical silhouette (projection along the drawing-plane normal,
  the prism's own axis). Closed bodies are untouched: they keep the true solid boolean, which is strictly
  better there (it rebuilds interior walls; a surface cut cannot). Works for the closed-loop *and* the
  open-line side-pick branch for free — both already build their ring in the same plane basis.
  ⚠ **TRAP (cost one wrong implementation): CENTROID IS NOT AN INTERIOR TEST.** The scribe leaves the
  surviving OUTER region as concave n-gons whose centroids land INSIDE the loop (plane case: all three face
  centroids read as enclosed) — the first version would have deleted the entire surface, saved only by the
  "never consume every face" guard. The rule is **every VERTEX inside-or-on the ring** (plus a centroid check
  to reject a sliver hugging the ring from outside). "On" must count as in — after a scribe every vertex of
  the enclosed region lies *exactly* on the ring, and `nurbs_trim::point_in_polygon` is a crossing-number
  test that coin-flips there; hence `knife_ring_contains_or_touches` with an extent-scaled epsilon.
  Gates: `closed_loop_subtract_punches_a_hole_in_an_open_plane` (area 4.0 → 3.0, outer extent untouched,
  boundary 4 → 8 = rim + hole), `open_mesh_subtract_never_consumes_the_whole_surface`,
  `closed_body_still_takes_the_solid_boolean_path`. 39 knife + 14 slice green.

- 2026-07-03: interior cut points (slit + zigzag); wrapping-cut fan (multi-crossing + direct-ring); last-point
  = commit hover snap; twin-corruption class killed via polygon-soup + `from_polygons` (5 adversarial stress
  families); Ctrl+Z pops last knife point; cross-plane face merge fixed via `plane_dist` routing.
- 2026-08-07: owner "slice ALWAYS fails to do a proper cut" — two fixes, one discovery, one new harness.
  (1) `knife_apply_slice_layers`: the front pass chorded hidden faces (`through=true`) that deeper layers then
  scribed exactly → every hidden face cut TWICE at densify-quantized offsets (dup ring verts, slivers, one
  mis-bound INTERIOR flap vert). Now the front pass chords only when no deeper layer exists (`deep_scribe`).
  (2) Engine crossing materialisation: 3D vertex snap at ~1% mesh diag — a crossing near an existing vertex on
  its edge (endpoint, previous split, earlier-pass split, near-edge densify snap) reuses it. Edge-PARAM
  epsilons (VEND) cannot do this: they don't compose across passes and densify chord error scales with sample
  step. Straight box slice: was +7v/+3f with an interior vert; now +4f clean ring, all dups gone.
  (3) Discovered classic-S face deletion (§6 P1, pre-existing). (4) New self-driving GUI harness
  `AZRA_KNIFEDIAG=free|classic` (visdiff.rs) — real input path (hover→click→S), per-face topology dump,
  screenshot; plus 2 new app-path tests (oblique orbit view, moved+rotated body — both previously untested
  axes, both pass).
- 2026-08-07 (later still): crossing-merge tolerance now capped per edge at 10% of the edge being split
  (owner ortho-cylinder feedback "a lot better but not perfect": 1% of MESH diag rivals short facet edges on
  long dense bodies — uncapped, legitimate crossings snapped to endpoints and the cut line degraded).
- 2026-08-07 (later): classic-S face-deletion P1 FIXED — THREE stacked causes, each now a §3 invariant.
  (a) Run walk paired nodes by raw path order, so a HIDDEN through-crossing (B8, right-back edge) interleaved
  between the visible chain's anchor (B9) and the front-face interior point and stole the tail-slit anchor →
  face-connectivity chain building (hidden crossings form their own depth-sheet chain; visible chain keeps
  B9→I(B)). (b) `split_poly_slit` bound that cross-face spur via footprint winding with plane distance only
  RANKING candidates — now a hard gate (>2% face diameter = reject). (c) The real face-eater:
  `purge_degenerate_faces` (2026-08-06 digon rule) treated ANY repeated ring vertex as degenerate and deleted
  every slit-carrying face on the classic commit — narrowed to <3 unique corners / zero-length edge.
  Result: two-face interior S = two slits anchored at the shared-edge crossing, f 6→6, `validate()` ok
  (Blender parity per §1). Regression test `s_classic_two_face_clicks_never_deletes_faces` (the classic
  personality's FIRST app-path S test). Free personality verified unchanged (+4-face ring).
- 2026-08-07 (owner "KNIFE FAIL.fl4", Blender-oracle gated): ROOT CAUSE of the rect-slice failure = the run
  walk treated a CLOSED path's node stream as LINEAR (§5.4 new). Owner's saved mesh: two 16-gons with
  repeated vertices (`5,37,5` and `6,31,29,31,6`) + 2 coincident clusters — i.e. the through-cut B5→B6 was
  split at the wrap into two dangling slits, each materialising its own copy of the shared rect corner.
  Reproduced deterministically through the REAL app path (`s_rect_notch_slice_has_no_duplicate_or_pinch`,
  poly_knife.rs): rect over a cube with its top side above the silhouette. `AZRA_KNIFE_DIAG=1` printed
  `I…B9 BRK B8 I…` → `head-slit` + `tail-slit`; after the fix `B8 I×13 B9 BRK` → one `through 4->5 span=13`.
  New oracle `knife_rect_notch_oracle.py` (EXACT DIFFERENCE of the equivalent prism): nv=16 nf=10
  pinched=0 coincident=0 boundary=0 non_manifold=0 arity{4:8,8:2} vol=0.760 — our slice now matches all
  four invariants and DIVIDES both notched sheets. All knife suites green.
- 2026-08-07 (owner "KNIFE CIRCLE FAIL TO DO, GOT SEGMENT AFTER CUT", `KNIFE CIRCLE CUT FAIL.fl4`) —
  DIAGNOSIS ONLY (`knife.rs` locked by a concurrent pinch/manifold session; patch delivered as text, §5.5).
  ROOT CAUSE: `knife_cut_screen`'s anchor-less branch (`knife.rs:722-739`) has NO closed-loop case (§5.5
  new, §5.6 trap). A circle wholly inside one face crosses no edge and snaps to no vertex → no Bound
  anchors; head and tail are the SAME point (the shape rebuild repeats the first sample so
  `knife_free_is_closed()` reads closure), so `nearest(head) == nearest(tail)` and the `(Some(a), _)`
  fallback calls `split_poly_slit`. Owner's mesh: v=67 f=29, `validate()` OK, `is_manifold()` =
  "degenerate face 0: repeated vertex 65", arity `{4:22,5:1,8:1,9:2,11:2,47:1}` — face 0's 47-ring has 28
  unique verts (box corners, bridge v8→v48, circle v48..v66, then the SAME circle backwards v65..v48). A
  spur; the face was never divided. 0 boundary, 0 wire, 0 orphan, 0 sliver, 0 valence-2 pin — the stray
  "segments" ARE the spur. Plus 1 coincident cluster `[v48,v66]` (the duplicated closing sample).
  Blender oracle `circle_knife_oracle.py` (cylinder DIFFERENCE box, EXACT): nv=40 nf=24 ne=64,
  non_manifold=0 boundary=0 wire=0 orphan=0 coincident=0 **pinched=0** arity `{4:20,7:2,17:2}` — the
  annulus arrives as TWO SIMPLE polygons, never a keyhole and never a spur. Target topology proven legal
  in our kernel by `core_math/tests/knife_circle_annulus.rs` (2 tests green: the annulus split rebuilds
  manifold with 0 pinched / 0 boundary; the slit form asserts non-manifold, locking the class). New
  loader diag `app/src/project/diag_circle_knife.rs` (boundary / wire / orphan / sliver / valence-2 pin /
  coincident / worst-ring dump) and new harness mode `AZRA_KNIFEDIAG=circle` (visdiff.rs) which censuses
  pinched rings and prints `VERDICT(circle)`. All knife suites still green (26+2 app, 24 core_math).
  Note: `bpy.ops.mesh.intersect` on a COPLANAR circle is NOT a usable slice oracle — it leaves the loop
  as a separate n-gon with 16 boundary edges and never touches the parent face.
- 2026-08-07 (later, `knife.rs` lock released): §5.5 patch APPLIED. New `split_poly_closed_loop()`
  (beside `split_poly_slit`) splits the carrier face into inner disc + two annulus halves, bridged at two
  non-crossing (ring, loop) vertex pairs, plane-GATED carrier selection (§3), midpoint-visibility bridge
  filter, loop re-oriented to the parent ring's winding via Newell, parent material propagated to BOTH new
  faces (poly/material arrays stay parallel, `debug_assert`). Call site in the anchor-less branch, before
  `let head = …`, `continue`s the chain. Deviation from the written patch: `is_loop` also accepts a loop whose
  closing sample was already deduped (`gap ≈ one step`, no `pop()`) — the 4-corner-in-one-cell case.
  EVIDENCE (real app path, `s_circle_inside_one_face_divides_it_and_never_pinches`, new): OFF → pinched rings
  `[25..41,40..25,3,2,5,4,3]` + `[0,8..24,23..8,0,7,6,1]` (owner's out-and-back spur, face never divided);
  ON → pinched 0, boundary 0, front sheet 1 → 3 faces. `knife_circle_annulus` grew 2 real-code-path tests
  (flat quad 1→3 faces; solid box +Z face, manifold, boundary 0, 6→8 faces) — both fail with the branch off.
  `AZRA_KNIFEDIAG=circle` at its default 3-segment ring never reached this branch (`inter.len() >= 4`), so it
  was measuring the anchored-chord path; the harness now defaults the circle to 16 segments
  (`AZRA_KNIFEDIAG_SEGS`) and prints `VERDICT(circle): CUT COMMITTED` with v=40 f=24 pinched=0 manifold=true —
  the same nv/nf as Blender's oracle (`nv=40 nf=24 ne=64`); bridge placement differs so the arity histogram
  does not, but all four census invariants match. `knife_stress_closedloop::closed_interior_single_face_
  splits_ngon` updated: a closed interior loop now yields +2 faces (disc + 2 halves), not +1 (chord) — the
  test's own comment already flagged the chord as "does not truly CLOSE". Also tightened the slice commit gate
  to `knife_result_is_sound()` (§3) — narrower than `is_manifold()` on purpose; see the §3 note for why the
  blanket check would have refused every legitimate slit.

- 2026-10-11 (owner *"when i try to k rectangular, pressing r do not trigger"*): NOT a dispatch bug —
  `knife_r_key_dispatch_tests` (poly_knife.rs) drive the real `handle_keyboard` with synthetic egui key
  events and prove `K` → `R` arms the rectangle from poly edit and from the sculpt room. The trap was the
  REPEAT chord: a shape cut keeps the knife armed (§ "shape should still be the same mode after a cut"),
  so the next `K` hit `poly_activate_knife`'s same-key branch, which cancelled the tool with NO toast
  (written 2026-07-18, when the knife still exited after every cut). `R` then had nothing to arm. Fix:
  same-key `K` = bevel/hinge convention — commit a cut in progress (anchored shape → `shape_release`,
  ≥2 free points → `poly_knife_confirm`), otherwise restart fresh as a free line and STAY armed
  (`poly_knife_shape_to_free_line` toasts). Dead ends ruled out: egui 0.29 multipass (second pass gets
  `new_input.take()` = empty), button focus stealing keys (only TextEdit/DragValue `request_focus`),
  a second R dispatcher (none), stale binary (release exe rebuilt 2026-10-11), user keybinds (empty).
