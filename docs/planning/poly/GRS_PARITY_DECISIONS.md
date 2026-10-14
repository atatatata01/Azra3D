# GRS_PARITY_DECISIONS.md — Poly⇄SDF G/R/S Transform Parity: THE DECISION RECORD (READ FIRST)

> **This is a DECISION record, not a proposal.** The owner ruled; recon, three fact-finders and two
> adversarial challenges ran; every contested claim below was re-verified against source by the integrator
> before it was written down. **D4, D5 (order), D10 and D11 CHANGED from the proposed set — a challenge
> landed on each.** If you are about to touch `compute_transform`, `poly_update_transform`, or the gizmo
> path, read §0 → §3 before writing a line, then execute §6 slices in order.
>
> **Companion docs:** `POLY_POLISH_PLAN.md` (§0 oracle protocol — the method used here), `CLAUDE.md`
> (the rules; §5 records one conflict found and how it resolves).

**Scope:** the G/R/S (grab / rotate / scale) transform math and its entry points — `app/src/transform.rs`
(`compute_transform`, the mandated single math path), `app/src/gizmo.rs` (the gizmo path),
`app/src/poly_drag_tools.rs` (the poly modal path), `app_types/src/types_transform.rs` +
`types_tool.rs` (the state types). **Out of scope:** the keymap redesign (§8), the SDF pivot defect (§7,
filed separately), the bevel (BEVEL.md is authoritative).

---

## §0. THE OWNER'S RULING (verbatim, non-negotiable)

> **"the g.r.s do not have same behavior as the sdf g,r,s THEY SHOULD BE THE SAME."**

**=> SDF is the REFERENCE. Poly migrates onto the shared path. This is not relitigated.**

Two riders, both derived from evidence *after* the ruling, neither contradicting it:

1. **"The same" means ONE code path, not "poly copies SDF's current behavior."** On two conventions
   (rotation sign uniformity, view-awareness) **poly is the more correct pillar** and SDF is the one that
   moves (§3, D5). Migrating poly onto SDF *as-is* would ship a commit where poly is strictly worse —
   forbidden by CLAUDE.md ("Never leave a functionality gap for even one commit").
2. **"The same" is unmeetable by today's rotate math in EITHER pillar.** `transform.rs:154` computes
   `angle = dx * rotate_speed` — no `atan2`, no pivot. This is filed separately (§7) and does not block.

---

## §1. THE ROOT CAUSE — an abandoned migration, and it has run BEFORE

`compute_transform()` (`transform.rs:73`) is the mandated single math path. Its own header, verbatim:

> `transform.rs:4-9` — *"WARNING: ALL transform math lives in compute_transform(). Do NOT add grab/rotate/scale calculations elsewhere."*

SDF obeys it. Poly was built later, cloned the already-condemned `start_modal_transform` shape, then was
**25% migrated and stalled**: `poly_update_transform` (`poly_drag_tools.rs:605`) calls `compute_transform`
**for GRAB ONLY** (`:624-634`), with a comment claiming *"ensures identical feel to SDF/node transforms"*
(`:621`). Rotate (`:637-652`) and Scale (`:655`) were left inline. **`poly_drag_tools.rs:630` hardcodes
`snap_active: false, free_mode: false`** — the fossil of the stall.

**THIS EXACT MIGRATION HAS ALREADY RUN ONCE, AND ITS OUTPUT IS THE FOSSIL WE ARE NOW DELETING.**
`input_viewport.rs:1278-1281`, verbatim:

> *"Previously used start_modal_transform() — a separate code path that updated from handle_keyboard instead of handle_viewport_input, causing choppy movement... Routing through the gizmo system eliminates the duplicate path."*

That is **D1, verbatim, already executed for prims**. Its result today: **`start_modal_transform`'s only
surviving caller is `input_keyboard.rs:943`, under `:937 // G/R/S for lights`.** Its prim branch
(`gizmo.rs:619-679`) and node branch (`:681-771`) — ~150 lines — are **unreachable, still compile, and
nothing ever flagged them.** §7 is the constraint that stops us building a third copy.

**There is a FOURTH implementation the recon missed:** `curve_point_edit.rs:743-749` — a 2D RH+ rotate on
`dx * settings.rotate_speed`. The migration surface is four sites, not three.

---

## §2. THE DECISION TABLE (D1–D12 + gate)

Legend: **KEEP** = proposed decision survives · **CHANGED** = a challenge landed, decision is different ·
**REJECT** = premise falsified.

| # | Decision | Verdict | Rationale + evidence |
|---|----------|---------|----------------------|
| **D1** | Poly updates from `handle_keyboard`, not `handle_viewport_input` → migrate onto the gizmo path | **KEEP** | `poly_update_transform` has exactly ONE caller: `input_keyboard.rs:443`, inside the poly keyboard block (`:420-443`). This is the identical defect `input_viewport.rs:1278-1281` documents fixing for prims ("choppy movement — `hover_pos()` returns None during drag on some frames"). Same disease, same cure. **No objection landed.** |
| **D2** | Ctrl-snap dead in poly (hardcoded false) → "fixed by migration" | **CHANGED — mislabeled, and scoped** | Not a restoration: poly sub-element Ctrl-snap **has never worked**, so migration makes it live *for the first time*. `transform.rs:133-135`: `snap_pos` **OVERRIDES the delta wholesale** — the result is not "nudge toward a target" but *the selection median teleports onto a smart-snap target*, every selected vert riding the jump; and the gizmo reads Ctrl **live every frame** (`gizmo.rs:1431`), so a stray Ctrl mid-drag teleports geometry. **DECIDED (not deferred to the owner):** plumb `snap_active` + `snap_grid` honestly — that is real parity **and** Blender-correct (Ctrl in edit mode = *increment* snap). Pass **`snap_pos: None`** for poly sub-element: smart-snap targets are derived by the **object-placement** snap system and have no sub-element target provider. **This is not a special-case bandaid — it is the absence of a data source**, and vertex/edge/face snapping is **already tracked as its own item** in `POLY_POLISH_PLAN.md §5` ("Snap-to-geometry (vertex/edge/face) for edit-mode transforms"). Do **not** re-hardcode `false` — that fossil is what got us here. |
| **D3** | `free_mode` is a dead field → delete | **CHANGED — half-false** | Dead in the **math** (`poly_drag_tools.rs:630` hardcodes `false`) but **LIVE in the DRAW**: `draw_gizmo.rs:276` (hides the gizmo during a free drag) and `:443` (suppresses gizmo hit-test). Field is real: `types_transform.rs:173`. **It MIGRATES to `GizmoInteraction`; it is NOT deleted.** Wiring it honestly **changes poly free-rotate feel** (see D5) — owner-visible, announce it. |
| **D4** | Shift+X plane-exclude broken in poly rotate/scale → "fixed by migration" | **REJECT — premise FALSE** | `compute_transform` **aliases `Plane*` → axis** for Rotate (`transform.rs:175-177`) and Scale (`:191-193`). Only **Grab** implements real plane math (`:125-127` `explicit_plane_delta`). **SDF has no plane-exclude for R/S either — migration cannot "fix" what does not exist.** Real effect: poly's Shift+X rotate goes from *silently ignored* (`_ =>` camera-dominant axis, `poly_drag_tools.rs:644-649`) to **X-axis**; Shift+X scale goes from **uniform** (`_ => local * scale_factor`, `:679`) to **X-only**. That is a behavior change, arguably a fix — **it is not plane-exclude.** **Tell the owner: "Shift+X in R/S = axis, not plane, in BOTH pillars."** Reclassified as new UI-track work, not part of this migration. |
| **D5** | Poly Z-rotation sign opposite to SDF → "poly matches SDF" | **CHANGED — substance under-specified, ORDER backwards** | See §3. Taken literally this ships **two regressions**. Restated: *"poly and SDF converge on ONE axis-uniform, view-aware sign — and `compute_transform` is fixed FIRST (S0), before poly migrates onto it."* |
| **D6** | Poly does world math on body-local verts → FIX FIRST | **KEEP — ACTIVE, and it is S1** | See §4. **CONFIRMED ACTIVE** via three independent user paths. Ladder rung **4** (workflow breakage), *not* 1–3: it corrupts no source of truth and undo/selection/save-load all work. It is **fix-first WITHIN the migration**, not a separate emergency — it lives inside the exact function S3 rewrites. |
| **D7** | Elongate silently aliased to Scale in poly → reject explicitly | **KEEP — PROMOTED to BLOCKING** | Today: `poly_drag_tools.rs:672` `TransformKind::Scale \| TransformKind::Elongate =>` — a silent alias, wrong *today*. **Post-migration it becomes worse:** `gizmo.rs:1216`/`:1426` map `GizmoMode::Elongate → TransformKind::Elongate`; `compute_transform`'s Elongate branch (`transform.rs:208-246`) returns `original_scale` **unchanged (ONE)** *and* a **non-zero `elongate_pos_offset`** folded into `result.position` (`:248`). A poly branch reading `(delta, factor)` would get `factor = ONE` (verts freeze) **and a bogus non-zero delta (verts TRANSLATE)**. Silent geometry nonsense. Reject at the poly entry (toast + no-op). |
| **D8** | Poly has numeric input, SDF does not → level SDF UP | **KEEP — but see the RULE CONFLICT (§5)** | Near-free: `numeric_value` **already exists** on `TransformInput` (`transform.rs:58`) and is honored by **all four** kinds; SDF simply passes `None` at `gizmo.rs:1467`, `:1514`, `:1529`. Emergent from S3, **not a separate slice** (§5, §6). |
| **D9** | Poly has mid-drag X/Y/Z, SDF does not → level SDF UP | **KEEP — but UNDERSCOPED** | `GizmoInteraction` **derives** axis from `component` (`gizmo.rs:1414-1419`) and has **no mutable axis field**. Requires a new **`axis_override: Option<AxisConstraint>`**. This is **the only structural add in the whole migration** — and it is **migration-MANDATORY** (it preserves poly's *existing* mid-drag X/Y/Z at `input_keyboard.rs:422-424`), not a gift to SDF. |
| **D10** | No poly HUD → add one | **REJECT — INVERTED** | **Poly HAS a full HUD.** `tool_settings.rs:193-195` (`is_poly_transform()` → title "Transform"), `:238-264` (kind + axis incl. "YZ plane" + `num_buf` + hint line), `viewport_hints.rs:127`, `workspace.rs:661-671` (toolbar axis readout). **`ActiveTool::Gizmo` has NO HUD — verified: `grep -c "is_gizmo\|as_gizmo" tool_settings.rs` = `0`.** Migrating poly onto `ActiveTool::Gizmo` **DESTROYS the poly HUD**. Action = **re-key the existing poly HUD onto the gizmo variant (SDF gains it for free)**. This is a **REGRESSION RISK**, not an enhancement. |
| **D11** | Poly pivot = median, SDF pivot = body origin → "keep both, not a bug" | **CHANGED — the one decision that was WRONG** | See §7. Premise false on **both halves**: AZRA **already ships a per-node pivot setting** the math throws away. **Restated:** *both paths resolve the pivot through the ONE existing resolver `selection_median()`; poly's hand-rolled local centroid is DELETED.* D11's **observation survives** (median for sub-element edit, pivot-resolved for object — Blender-correct); its **instruction does not**. |
| **D12** | Global/Local decorative in both | **KEEP — out of scope, UI track** | No objection landed. `space` is carried on `GizmoInteraction` and not honored by the math. Not this task. |
| **GATE** | Alt+LMB workplane free-move dead in poly via `!in_sub_edit` → lift the gate | **KEEP — with a mandatory alignment** | Premise is **narrower than stated**. `input_viewport.rs:1258-1276` **already** routes poly to free-move when the gizmo is **armed** and you click empty space (`:1261` → `poly_start_transform` → `:1270 ts.free_mode = true`). The Alt+LMB branch (`:1282`) is an `else if` **below** it — so Alt+LMB is dead in poly **only when the gizmo is NOT armed**. Lifting `!in_sub_edit` routes poly into `start_gizmo_drag(Center)` → the poly intercept (`gizmo.rs:1207-1229`), which sets `axis` + `confirm_on_release` but **NOT `free_mode`** (stays `false`, `poly_drag_tools.rs:600`). **Post-lift, two poly free-move entries disagree on `free_mode`.** Today that is cosmetic (`draw_gizmo.rs:276`/`:443`). Once D3 wires `free_mode` into the math, **the same user gesture yields two different rotations.** **MANDATORY: lift the gate and make both entries agree (Center / empty-space / Alt ⇒ `free_mode = true`) in the SAME slice, before wiring it into `TransformInput`.** |

---

## §3. THE ROTATION-SIGN ORACLE FINDING — and why D5's ORDER flipped

### The oracle (hard data)

Blender 4.5, `-b --factory-startup`. Method: cube, `bpy.ops.transform.rotate(value=radians(90),
orient_axis=A, orient_type='GLOBAL')`; dump `matrix_world @ probe`; control = `mathutils.Matrix.Rotation`
(the right-hand-rule reference) dumped in the same run.

```
orient_axis='Z', value=+90°:  X̂ → (-0.0, -1.0,  0.0)    euler → (0, 0, -90)
orient_axis='X', value=+90°:  Ŷ → ( 0.0, -0.0, -1.0)    euler → (-90, 0, 0)
orient_axis='Y', value=+90°:  Ẑ → (-1.0,  0.0,  0.0)    euler → (0, -90, 0)
RH control, Matrix.Rotation(+90,'Z'):  X̂ → (0.0, +1.0, 0.0)
```

**⇒ Blender's `transform.rotate` negates a positive `value` relative to the right-hand rule — on ALL THREE
AXES, IDENTICALLY. Blender has NO special Z case.**

### Is SDF itself wrong? **YES — two ways.**

1. **Ad-hoc Z-negation.** `transform.rs:175` X → RH+, `:176` Y → RH+, **`:177` Z → `-angle` = RH−.**
   Exactly one of three axes is "Blender-signed" — **the signature of an ad-hoc flip added to make one case
   look right, not a derived correction.** A genuine handedness fix would never single out Z. And it cannot
   be an egui-Y-down artifact: Y-down enters **only** as `-d.dot(up)` (`types_gizmo.rs:173`/`:181`), in
   camera space, and **the rotate branch never touches screen projection** — it reads only `dx` and
   `cam.cam_fwd`.
2. **View-blindness.** `perpendicular_axis()` (`types_gizmo.rs:90-99`) takes **`.abs()` of all three
   components** and returns an **unsigned** `GizmoComponent` — it **structurally cannot know which side the
   camera is on**. `transform.rs:168-172` are literal constants. **Orbit 180° → sign unchanged → the object
   turns opposite to the mouse.** The dead `free_mode` arm (`:166`) carries the *identical* defect plus its
   own `.abs()`.

### **POLY IS THE MORE CORRECT PILLAR on both counts**

- **View-aware:** `poly_drag_tools.rs:644-649` signs the free-rotate axis by the view —
  `Vec3::Z * cam_fwd.z.signum()`. Camera at +Z → drag right = CW; camera at −Z → **still CW**.
  Screen-consistent from both sides. SDF's `_` arm gives CW from +Z and **CCW from −Z**.
- **Axis-uniform:** `poly_drag_tools.rs:641-643` = `Vec3::X / Y / Z`, all **RH+**. SDF = RH+/RH+/**RH−**.

### **DECIDED**

**`compute_transform` adopts poly's conventions, and it happens in S0 — BEFORE poly migrates.**

- **Sign: all three axes RH+ (drop the `-` on `transform.rs:171` and `:177`).** Chosen over
  Blender-parity (negate all three) because: (a) it is the **cheaper, behavior-NEUTRAL migration** — poly is
  *already* RH+ on all three, so poly's explicit-axis rotate does not change at all, and only SDF's R,Z
  flips; (b) uniform beats ad-hoc; (c) Blender's negation is an **operator-argument** convention, and
  Blender's *modal drag* applies a view flip on top of it — so it does not bind a mouse-drag convention.
- **View-awareness: adopt poly's `signum()` idea** for the unconstrained arm. This is a **level-UP**, exactly
  like D8/D9 — **the reference direction is not uniformly SDF→poly.**

### Why the ORDER flipped (this is the load-bearing part)

The proposal said "unify first, flip once, in one place." It is right that one site beats four. **But
migrating poly onto SDF's rotate as-is ships a commit where poly is strictly WORSE** — it *loses*
view-awareness and *gains* sign asymmetry. **CLAUDE.md, Feature Dependency Audit, verbatim: "When replacing
a system, implement the replacement BEFORE removing the original. Never leave a functionality gap for even
one commit."** Unify-then-flip leaves exactly that gap.

**The decisive argument is testability.** Per `feedback_verify_app_path_not_kernel`, **the owner is the only
oracle for rotate feel** (the running app cannot be observed from the agent side). **Unifying first DESTROYS
poly's view-aware rotate — the very reference the owner would judge the new sign against.** Fix
`compute_transform` first and the owner can A/B the changed SDF rotate against poly's still-working one.
Once poly is migrated, **that reference is gone forever.**

S0 is cheap (a handful of lines in one function), fixes SDF at the same time, and then poly migrates onto an
already-correct path and never regresses.

### Filed separately (does NOT block — see §7)

**Rotate has no `atan2` and no pivot.** `transform.rs:154`: `let raw_angle =
input.numeric_value.unwrap_or(dx * settings.rotate_speed);` — `dx` **only**. `dy` (`:81`) is computed and
**never read** by the rotate branch. **Dragging straight up/down rotates nothing**, and dragging right
yields the same angle whether the cursor is above or below the pivot — in **both** pillars **plus**
`curve_point_edit.rs:743-749`. **No sign choice fixes this.** Unification makes it a **one-site** fix
instead of four; that is a reason to unify, not a reason to delay.

---

## §4. D6 — VERDICT: **ACTIVE**, with repro

**A poly body's transform lives on `SceneNode` (payload-agnostic), and every OTHER poly subsystem applies
`world_transform` correctly — only `poly_update_transform` does not.**

**Storage (non-identity is structurally possible):** `scene_graph.rs:519-521` — `local_position` /
`local_rotation` / `local_scale` live on **`SceneNode` itself**, not the payload. `NodePayload::PolygonalBody`
(`:368-375`) has name/mesh/mesh_cache_id/material/modifiers — **no transform fields**, which is exactly why
the recon missed this. `world_transform` (`:858-874`) composes up every ancestor and **never inspects the
payload type**.

**Three independent user paths, no identity guard anywhere:**
1. `properties_poly.rs:154` — the poly **Transform** header, **`default_open(true)`**, Pos/Rot/Scl sliders
   writing `n.local_position` / `local_rotation` / `local_scale`. Zero friction.
2. `gizmo.rs:1257` — the **non-SdfBody** branch: `(n.local_position, n.local_rotation, n.local_scale)` →
   `node_targets` → `apply_to_scene_node`. Object-mode gizmo drag on a poly body.
3. `creation.rs:343` — placement rides the node transform.
Plus **parenting** (a poly body under a transformed Empty is non-identity with its own transform clean).
**Nothing resets it** (no freeze/bake op exists) and it **persists through save/load** (`scene_graph.rs:512`,
plain non-skipped serde fields). The tool gate (`gizmo.rs:1208-1218`) checks only context/mode/selection —
**no identity check.**

**Everything else is world-correct — this is what makes it a bug, not a convention:** render
(`raster_gbuffer.rs:336`), picking (`poly_selection.rs:108-112`, `:340-343`), gizmo placement
(`selection.rs:147`), and `poly_join` (`boolean.rs:561`/`:612`). **`boolean.rs:545`'s own doc comment says
the target "keeps its own transform"** — non-identity poly transforms are not something the code merely
tolerates, they are a state it is **written to expect**. **Broken ONLY at** `poly_drag_tools.rs:522-525`
(pivot from raw local verts), `:626` (local center fed as `original_pos` against the **world** camera), and
`:663` (world delta added onto local verts).

**Picking is NOT affected** — the blast radius is narrow: `poly_update_transform` only. This *sharpens* the
symptom: the user picks the **right** vertex, sees the gizmo in the **right** place, then the drag misbehaves.

### The math (why the ratios are falsifiable numbers, not vibes)

Let `M = T·R·S` be the body world matrix, `A = R·S` its linear part. Verts stored local, displayed as `M·p`.

- **GRAB:** code writes `p' = p + d` (`d` world). Displayed `M·p' = p_w + A·d`; desired `p_w + d`. **Wrong iff `A ≠ I`.**
- **ROTATE** about local centroid `c` by `Q`: displayed `c_w + A·Q·A⁻¹·(p_w − c_w)`. **The pivot is CORRECT**
  (`c` maps to `c_w`); the effective **axis** is `A·axis`. Wrong iff `R ≠ I`; with non-uniform `S`,
  `A·Q·A⁻¹` **is not even a rotation → SHEAR**.
- **UNIFORM SCALE:** `M·(c + s(p−c)) = c_w + s(p_w − c_w)` **exactly, for ANY M**. ⚠️ **Uniform scale is the
  ONE op that is immune — the migration must NOT regress it** (a naive world-round-trip gets this wrong, and
  it is the one case that silently looks "already fine" before the change). **Add a regression check.**
- **DEFAULT PLAIN G** (`axis = None` → `transform.rs:103` `explicit_plane_delta(cam.plane_normal)`):
  `compute_plane_delta` (`:300-301`) intersects the **world** camera ray with a plane through `object_pos`;
  the delta scales with `n·(P − ro)`. Feeding local `P_l` scales the delta by
  `n·(P_l − ro) / n·(P_w − ro)`. **Wrong iff the translation has a component along the workplane normal —
  EVEN WHEN `A = I`.** So **pure translation — the most common case, exactly what the object gizmo
  produces — breaks the default G.**

### REPROS (run these; they are falsifiable — if a human sees 1:1 tracking in REPRO A, this verdict is wrong)

**REPRO A — translation breaks default G (predicted 1.5× overshoot):**
1. Create a poly cube. Object mode.
2. Properties → Transform → **Pos Y = 5** (or gizmo-drag it up 5).
3. Top view looking down −Y, camera distance **15** → cam at (0,15,0); Auto workplane resolves XZ, `n = (0,1,0)`.
4. Tab → Vertex mode. Click a vertex (**picking is correct** — it selects the right one). **G**, drag.
5. **PREDICTED: the vertex tracks at 1.5× the cursor and outruns it**, gap growing with drag distance.
   `n·(P_l − ro) = −15`; `n·(P_w − ro) = −10`; ratio **15/10 = 1.5**.
6. **Pos Y = 10 → 15/5 = 3×.** Error scales as `15/(15 − Y)` — **tunable ⇒ a measurable confirmation.**

**REPRO B — rotation, most visually damning (predicted 90° off):**
1. Poly cube → **Rot Y = 90**. Tab → Vertex mode, select a vertex.
2. **G** then **X** (constrain to world X).
3. **PREDICTED: the vertex moves along world −Z (into/out of screen), NOT along X** — `R_y(90°)` maps
   `(1,0,0) → (0,0,−1)`. **The geometry travels perpendicular to the axis guide the user is looking at.**
   Same with **R + X** → spins about world −Z.

**REPRO C — scale (predicted exactly 2×):**
1. Poly cube → **Scl = (2,2,2)**. Tab → Vertex mode, select vertex, **G**, drag.
2. **PREDICTED: exactly 2× the cursor delta** (`A·d = 2d`).
3. **Control:** a plain **S** (uniform scale) on the SAME body is **CORRECT** — that contrast confirms the
   `A·d` mechanism rather than a generic speed bug.

### The fix shape (**NOT** "convert the pivot to world")

Verts must stay local, so: **run the math entirely in world (world pivot, world camera, world delta), then
map back with `M⁻¹` per vert.** A "just world-ify the pivot" patch fixes REPRO A and **leaves B/C broken** —
their error is `A·d` on the **delta**, not the pivot.

**The precedent already exists in the file we are migrating INTO:** `gizmo.rs:1282-1296` converts prim
local→world **before** `compute_transform` (`bt.local_to_world`); `cancel_gizmo_drag:1639-1649` converts
world→local **back** (`bt.world_to_local`). Copy that shape.

**REUSE (CLAUDE.md "No Duplication — Search Before Writing"):** `poly_start_transform:517-530` hand-rolls a
centroid from raw local verts, **duplicating `poly_selection_center()` (`selection.rs:138-151`)**, which
already computes exactly that centroid **correctly, in world space** (`:147` applies `world_transform`), and
which **the gizmo already uses to draw itself**. **Reuse it — do not fix the duplicate.** That also
**structurally guarantees the gizmo and the math share one pivot**, which is the root of D6.

---

## §5. ⚠️ CLAUDE.md RULE CONFLICT — surfaced, not silently overridden

**Per CLAUDE.md "Rule Conflict Check": *"Never silently override a CLAUDE.md rule — even an owner-written
prompt (rules may be stale, or the prompt mistaken)."* Reporting it here as required.**

- **The rule:** *"**Scope Control — Refactor budget:** bug fix = max 1 local extraction + 1 fix.
  **Architecture task = refactor only, no new features.**"*
- **The conflicting part of the plan:** **D8/D9 + proposed slice S5** — "SDF gains numeric input + mid-drag
  X/Y/Z." This is framed as an architecture task (unify onto `compute_transform`) and hands the SDF pillar
  two capabilities it does not have. **On a literal reading: banned.**
- **Which appears wrong: NEITHER — the plan's *framing* is wrong, and that is what created the conflict.**

**Resolution: PROCEED, and DELETE S5 as a slice.**

The rule's intent — *don't let a refactor grow into a feature sprint* — is **not** violated, because
**D8/D9 are not additive features; they are EXISTING POLY CAPABILITIES whose preservation on a unified path
necessarily generalizes:**

1. Poly **has both today**: `input_keyboard.rs:422-424` (mid-drag X/Y/Z), `:426-440` (numeric buffer).
2. **CLAUDE.md's own "never leave a functionality gap for even one commit" FORCES S3 to carry both onto
   `GizmoInteraction`** — which has neither (`axis` is *derived* from `component` at `gizmo.rs:1414-1419`;
   `numeric_value` is hardcoded `None` at `:1442`, `:1467`, `:1514`, `:1529`).
3. Once `axis_override` and the numeric buffer exist and the keyboard block reads `GizmoInteraction`,
   **there is no `ActiveTool` variant left to distinguish poly from SDF — both are `ActiveTool::Gizmo`.**
   **Suppressing the features for SDF would require ADDING a guard** purely to preserve the asymmetry
   **the owner explicitly ordered removed** — i.e. *re-creating the fossil*, costing extra code **and**
   contradicting the ruling.

**⇒ The features arrive BECAUSE unification is the point. "Plumb the buffer, ~3 call sites" is what
mis-framed emergent behavior as separate work. S5 is DELETED; "SDF now accepts numeric input + mid-drag
X/Y/Z" becomes an S3 ACCEPTANCE CRITERION, not a work item.** If the owner later decides it is genuinely
work, it is a **separate TASK** — never slice 5 of this one. **Record this weighing in the S3 commit message.**

**Second, lesser conflict — declined as a false alarm (recorded for honesty):** S1 = D6 fix + mirror
world/local co-fix + `poly_selection_center` reuse, vs *"bug fix = max 1 local extraction + 1 fix."* **The
mirror is NOT separable:** `poly_drag_tools.rs:710`/`:723` reflect a **LOCAL** delta across a **WORLD**
normal (`types_mirror.rs:143` — "Set plane from a world axis"), so **fixing D6 alone would SHIP A NEW BUG**
(symmetry skews on any rotated body). That is **one fix with two call sites, not two fixes.** And the whole
effort is **the assigned architecture task**, so ladder rung 6 permits it. **S1 stands as scoped.**

---

## §6. THE GATED SLICE ORDER (final)

> **Verification gate, every slice:** `cargo build -p app` · and per `feedback_verify_app_path_not_kernel`,
> **verify in the FULL app path (input → tool → kernel → to_raster_data → render), NOT core_math unit
> tests.** Each slice is **its own task and its own commit** (this answers "Fix ONE bug per task" against a
> ~13-item set).

### **S0 — Fix `compute_transform`'s rotate: sign + view-awareness. (SDF-only; poly untouched.)**
`transform.rs:171`, `:177` → drop the `-` on Z (**all three RH+**). `:166`, `:168-172` → adopt poly's
view-aware `signum()` for the unconstrained arm.
**WHY FIRST:** unify-then-flip **ships a commit where poly is strictly worse** (CLAUDE.md: never leave the
gap) **and destroys poly's view-aware rotate — the only reference the owner can A/B the new sign against**
(§3). Cheap, fixes SDF now, and poly then migrates onto an already-correct path.
**Owner-visible:** SDF's **R, Z now spins the other way**; SDF free-rotate is now view-consistent.
**Behavior-NEUTRAL for poly** (already RH+ on all three).

### **S1 — D6: world/local + the mirror co-fix + REUSE `poly_selection_center`.**
Run the math in world; map back with `M⁻¹` per vert. Delete the hand-rolled centroid
(`poly_drag_tools.rs:517-530`) in favor of `poly_selection_center()` / `selection_median()`.
**WHY BEFORE UNIFY — and this survived a direct attack.** The challenge argued *"`update_gizmo_drag`
ALREADY does local↔world at `gizmo.rs:660-665` / `:813-824`, so S1 is duplicated work."* **The premise is
false, twice:** those lines are inside **`start_modal_transform`** (fn at `:618`) and
**`cancel_modal_transform`** (fn at `:772`) — **the condemned fossil** — and **`update_gizmo_drag`
(`:1396-1570`) contains ZERO conversion.** The gizmo path's real architecture is: convert **once at
snapshot** (`:1282-1296`), **pure world math in update**, convert back **at write** (`apply_to_sdf_prim:49-54`)
and **at cancel** (`:1639-1643`). **Nothing at the destination to inherit — and what IS there does not fit
poly:** `apply_to_sdf_prim:59` writes `result.rotation`/`.scale` **raw**, because a prim stores rot/scale as
separate fields the body composes at serialize time. **Poly has no per-vert rotation field — verts ARE the
geometry, so rotation must be applied to positions as `A⁻¹·Q·A`.** No such code exists anywhere.
**The conversion MOVES; it is never written twice.**
**The second reason is decisive: unify-first is UNVERIFIABLE.** S1 has a **falsifiable gate today**
(REPRO A/B/C: **1.5× / 3× / 90° / 2×**) against the shipping G/R/S keys. Unify-first confounds every
SDF-parity A/B diff against a **feel judgment only the owner can render**. A slice whose exit criterion is
*"owner says it feels the same"* **must not ship with a known space bug inside it.**
**REGRESSION CHECK (mandatory):** **uniform scale is currently CORRECT under any body transform** (§4) —
**do not regress it.**

### **S2 — D7: reject Elongate on poly explicitly (toast + no-op).**
Standalone and independently verifiable: the alias (`poly_drag_tools.rs:672`) is **wrong today**, before any
migration. Loud, cheap, and removes a migration-created silent-corruption path in advance.

### **S3 — THE UNIFY. (The dangerous slice — read §7 before writing a line.)**
- Rotate + scale onto `compute_transform` inside the **poly branch of `update_gizmo_drag`**, using the
  identity-originals trick already proven at `poly_drag_tools.rs:626`/`:634`:
  `original_pos: center_world, original_rot: ZERO, original_scale: ONE` ⇒
  **delta** = `result.position − center` · **quat** = `Quat::from_euler(YXZ, rot.y.to_radians(),
  rot.x.to_radians(), rot.z.to_radians())` (exact — `transform.rs:175-177` emits one non-zero component) ·
  **factor** = `result.scale` (**axis-masking is ALREADY done at `transform.rs:191-193` — this DELETES poly's
  manual match at `:675-680` and `:697-702`**).
- **Poly branch `return`s early** (like the light branch at `gizmo.rs:1455`) carrying
  `poly_sync_mesh_cache(nid)` + `mark_dirty(RENDER)` — it must **NOT** fall through to the
  **unconditional** `prim_buf_stale = true` at `:1568` (a needless prim re-upload every drag frame, and it
  does **nothing** for poly, which renders from the mesh cache: `mesh_sync.rs:27-49`). Same for
  `finish_gizmo_drag` — early-return **before** `rebake_no_undo()` (`:1598`), an SDF bake meaningless for poly.
- **Port the cancel vert-restore — SAME COMMIT** (§7).
- **Re-key the poly HUD** (`tool_settings.rs:193`/`:238-264`, `viewport_hints.rs:127`, `workspace.rs:661-671`)
  onto the gizmo variant. **SDF gains a HUD for free.** (D10.)
- **Migrate `free_mode`** to `GizmoInteraction`; re-key `draw_gizmo.rs:276` / `:443`. (D3.)
- **Add `axis_override: Option<AxisConstraint>`** + the numeric buffer to `GizmoInteraction`; hoist the poly
  keyboard block (`input_keyboard.rs:420-443`) minus its `poly_update_transform` call into a
  **variant-agnostic** block reading `GizmoInteraction`. (D9/D8.)
- **DELETE `ActiveTool::PolyTransform` + its accessors** (§7 — **hard constraint**).
- **ACCEPTANCE CRITERIA (not work — §5):** SDF now accepts numeric input and mid-drag X/Y/Z.
- **OWNER-VISIBLE FEEL CHANGES TO ANNOUNCE:** Shift+X in R/S becomes **axis, not plane** (D4); poly
  free-rotate becomes **camera-blended 3-axis** (`transform.rs:163-166`) instead of today's signed dominant
  axis (see S4 — path B only).

### **S4 — D2/D3 honestly wired + the GATE.**
Plumb `snap_active` + `snap_grid` (`snap_pos: None` for sub-element — D2). Wire `free_mode` into
`TransformInput`. **Lift `!in_sub_edit` (`input_viewport.rs:1282`) AND align both free-move entries on
`free_mode = true` in this same slice** (GATE) — otherwise D3 turns a cosmetic inconsistency into **two
different rotations for the same gesture**.
**WHY LAST:** wiring `free_mode` is the one change that alters math on **path B only** (see below).

### ~~S5~~ — **DELETED.** Emergent from S3 (§5). Not a slice.

### The two entry points: **path B changes, path A does not**
- **A) `gizmo.rs:1207-1229`** — gizmo handle. Sets `axis` from `comp`, `confirm_on_release = true`, leaves
  `free_mode = false`. **Unchanged.**
- **B) `input_viewport.rs:1258-1272`** — gizmo-armed + click empty space. Sets `free_mode = true` (`:1270`),
  leaves `axis = None`. **CHANGES.** Both are identical today only because `:630` hardcodes
  `free_mode: false`; the **only** live difference is cosmetic (`draw_gizmo.rs:276`/`:443`).
  Once honestly plumbed, `AxisConstraint::None` diverges: **Grab identical** (`transform.rs:103` ignores
  `free_mode`) · **Scale identical** (`:190` ignores it) · **ROTATE diverges** — `free_mode = true` gives a
  **3-axis camera-blended euler** (`:166`) vs the snapped single axis (`:168-172`), and poly today uses a
  **third** rule (signed dominant axis, `poly_drag_tools.rs:645-649`). **Off-axis views WILL feel different.
  Tell the owner before shipping.**

---

## §7. WHAT A STALL MID-MIGRATION LOOKS LIKE — **the hard constraint on S3**

**It looks like the fossil we are deleting. Identical bug, identical mechanism.** §1 proved this migration
**already ran once** (`input_viewport.rs:1278-1281`) and left ~150 unreachable lines at `gizmo.rs:619-771`
that **still compile** and that **nothing ever flagged**.

**THE FOSSILIZER — `app_types/src/types_tool.rs:279-283`:**
```rust
pub fn as_poly_transform(&self) -> Option<&PolyTransformState> {
    match self { Self::PolyTransform(t) => Some(t), _ => None }
}
```
**That `_ => None` is the mechanism.** Stop constructing the variant and **every** call site **silently
returns `None` forever, with ZERO compile errors**. Verified by grep: **24 sites across 10 files** —
`draw_gizmo.rs` · `gizmo.rs` · `input_keyboard.rs` · `input_select.rs` · `input_viewport.rs` ·
`poly_drag_tools.rs` · `tool_mode.rs` · `tool_settings.rs` · **`viewport_hints.rs`** · `workspace.rs`.
**Every one is `is_poly_transform()` (bool) or `if let Some = as_poly_transform()`. NOT ONE is an exhaustive
match arm. The compiler is blind.**

### **HARD CONSTRAINT ON S3 (non-negotiable)**

> **DELETE `ActiveTool::PolyTransform` (`types_tool.rs:57`) AND its accessors (`:179`, `:279`, `:282`) —
> do NOT orphan them.**

Deleting **breaks all 24 sites**, so **the compiler enumerates the migration surface for you** and a stall
becomes a **RED BUILD** — which CLAUDE.md **already forbids shipping** (*"cargo build -p app after EVERY
change... If it breaks, fix IMMEDIATELY — don't pile changes on a broken state"*). **A red build is a SAFE
stall. A silently-dead accessor is the fossil.** ⚠️ **This is a CROSS-CRATE edit** — the types live in the
**`app_types`** crate, not `app/src`.

### The cancel gap — **migration-CREATED silent corruption, Priority Ladder #1 AND #3. CONFIRMED.**

`cancel_gizmo_drag` (`gizmo.rs:1601-1655`) restores **only** `light_targets` (`:1617-1623`), `node_targets`
(`:1630-1633`), `prim_targets` (`:1637-1652`). **There is no poly branch and no vert restore.** A poly
`ActiveTool::Gizmo` with all three vecs empty **falls straight to `:1654`**:
`self.tool.current = Idle; ... self.cancel_episode(); self.rebake_no_undo();`
**⇒ the episode rolls the undo stack back while the VERTS STAY MOVED. Graph and undo stack disagree. Esc
mid-drag silently corrupts geometry.**

**Port from `poly_drag_tools.rs:742-756`:** three vecs (`original_positions`, `all_vert_originals`,
`mirror_originals`) **+ `mesh.recompute_normals()` (`:754`)**, **ORDER MATTERS** — verts restored **BEFORE**
`cancel_episode()` (`:758`), then `poly_sync_mesh_cache(nid)` (`:760`). Insert **ahead of the `:1654`
fallthrough**.

**IT LANDS IN S3, THE SAME COMMIT — NOT BEFORE IT.** The facts proposed "in or before the unify commit."
**Before is wrong:** a poly branch in `cancel_gizmo_drag` added while poly is still never `ActiveTool::Gizmo`
is **unreachable by construction** — you would be **hand-building the exact artifact this task exists to
delete**, and a stall right there leaves a permanent fossil **that reads as intentional**. CLAUDE.md's
"never leave a functionality gap for even one commit" is satisfied by **SAME-commit**, not by BEFORE-commit.

### The SDF pivot defect — **CONFIRMED, and FILED SEPARATELY (do NOT fold into this migration)**

**This is D11's real finding, and it is a shipped bug on the SDF side today.** AZRA **already has** a
per-node pivot setting: `PivotMode { BoundingCenter (default), Custom, WorldOrigin }`
(`scene_graph.rs:477-485`), doc at `:488` verbatim: *"Applies to ANY scene node — body, group, poly, nurbs,
empty."* User-reachable and undoable via a shipped **D-key pie menu** (`pivot.rs:174-269`, 6 placements,
inside `transaction("Set Pivot")`). **`selection_median()` (`selection.rs:61`) is the ONE resolver that
honors it** (`:73-83`) **and already contains the poly branch** (`:63-64` → `poly_selection_center()`). The
gizmo is **DRAWN** at it (`draw_gizmo.rs:288`, `:450`) and it **IS captured** into every
`GizmoInteraction.median_start_pos` (`gizmo.rs:1199`, `:1271`, `:1303`, `:1329`, `:1389`).

**Then the math throws it away. Grep is decisive:** `pivot` appears **0 times** in `transform.rs`, **0 times**
in `poly_drag_tools.rs`; **`node.pivot` / `.pivot.mode`: 0 times in `gizmo.rs`.** Instead
**`gizmo.rs:1475` applies `median_pos` ONLY `if multi_body` (`node_targets.len() > 1`)** — *a single node
rotates about `nt.original_pos`, ignoring the pivot entirely* — and **`gizmo.rs:1498-1505` hardcodes the
SdfBody `origin`** for the prim branch.

**REPRO (shipped today, no migration needed):** select an SDF body → **D** → **"Origin"** →
`pivot.mode = WorldOrigin` → **the gizmo JUMPS to world (0,0,0)** (`selection.rs:75`; with
`BODY_LOCAL_PRIMS = true` the body routes to `node_targets`, `gizmo.rs:1246-1275`) → press **R** → **the body
spins about its own origin, not the drawn gizmo.** **Gizmo drawn at one point, math about another — the exact
D6 defect class, on the SDF side.**

**Blender confirms ONE rule, not two** (oracle, 4.5 `-b --factory-startup`): `transform_pivot_point` is a
**single scene-level enum** — `['BOUNDING_BOX_CENTER','CURSOR','INDIVIDUAL_ORIGINS','MEDIAN_POINT',
'ACTIVE_ELEMENT']`, default **MEDIAN_POINT**, **shared across modes** (set CURSOR in Object → reads CURSOR in
Edit). **P2:** a cube whose geometry centroid sits at local (5,0,0) but origin at (0,0,0) → after R90 Z the
**origin is UNCHANGED** ⇒ pivot = the **origin**, not the geometry median. **P3:** two cubes at x=0 / x=10 →
after R180 Z they **SWAP** ⇒ pivot = **(5,0,0) = median OF ORIGINS**. **P4:** Edit mode, top 4 verts (median
z=1), S 2× → **top verts stay at z=1.0** ⇒ pivot = the **selection median**, not the object origin.
**⇒ MEDIAN_POINT = "median of the selected elements"; in Object mode the elements ARE origins.** One rule,
two granularities — **exactly what `selection_median()` already encodes.**
**ORACLE HONESTY:** **P6** (INDIVIDUAL_ORIGINS, multi-object) returned the **same** swap as P3's
MEDIAN_POINT — **proving background-mode `bpy.ops.transform.*` does NOT read
`tool_settings.transform_pivot_point`**; **P7** (`center_override=(5,0,0)` → A moved to (10,0,0)) proves the
operator *does* support an arbitrary pivot, so **P6 is a bg-exec limitation, not a missing feature.**
⇒ **P2/P3/P4 measure Blender's MEDIAN fallback** and establish MEDIAN_POINT's semantics correctly, but they
do **NOT** independently prove the setting is honored per-mode — **only P1's cross-mode property read carries
that claim.** Modal transform is not drivable headlessly.

**THE DECISION — how this splits (this is where the challenge landed and where it is bounded):**
- **IN THIS MIGRATION (S1):** poly's hand-rolled local centroid (`poly_drag_tools.rs:517-530`) is **DELETED**
  and replaced by the existing resolver. **D11's instruction — *"the unified poly branch must keep computing
  its own median center"* — is REJECTED:** it (a) duplicates `poly_selection_center()` (**CLAUDE.md REUSE
  RULE**), (b) **IS the D6 root** (local vs world), and (c) **structurally locks poly out of `PivotMode`
  forever.** **D11's substance survives:** median for sub-element edit, pivot-resolved for object — that is
  Blender-correct, and it is **already what `selection_median()` does** (`:63-64`). *And the instruction's
  other half was right for the wrong reason:* poly must not inherit `body_pivot` — **because `body_pivot` is
  itself the bug.**
- **FILED SEPARATELY (NOT this task):** the SDF-side pivot defect — `median_pos` ignored unless `multi_body`
  (`gizmo.rs:1475`), `body_pivot` hardcoded (`:1498-1505`), and **a SECOND verbatim copy of the same
  body-pivot orbit block at `gizmo.rs:1096-1136`.** **Scope Control: "Fix ONE bug per task."** The assigned
  task is the poly migration. **This is a real, shipped, owner-visible bug and it must be filed in
  `COMPLETE_BUG_TRACKER.md` — it is not this slice.** The lens's fear that D11 "changes scope materially" is
  **unfounded**: the feature exists, is wired to a pie menu, and is drawn — **only the math is missing**, and
  the poly half of the fix is a **deletion**.

---

## §8. THE DEFERRED BINDING — Alt+Ctrl / Alt+Shift "drag along normal"

**DEFERRED. Not this task. The deferral was attacked and SURVIVED — deferring is CHEAPER, not more expensive.**

**Why it cannot ride here: both modifiers are TAKEN.**
- **Ctrl = smart snap**, read **LIVE every frame** (`is_snap_active(ctrl)`, `gizmo.rs:1431`), and snap
  **OVERRIDES the delta** at `transform.rs:133-135`.
- **Shift = `shift_duplicate`**, the 3rd arg of `start_gizmo_drag`.

**Why deferring is cheaper (the counter-argument, answered):** the objection — *"the migration touches the
same call sites the binding will touch (`start_gizmo_drag`'s shift arg, `update_gizmo_drag`'s ctrl/shift
params, `input_viewport.rs:1282-1283`), so deferring costs a second pass"* — is **factually right about the
overlap and backwards about the inference.** A second pass over those sites **AFTER** unification is a pass
over **ONE path**. Designing the binding **NOW** means designing it against **FOUR divergent
implementations** (SDF gizmo, poly modal, the dead `start_modal_transform` shape, and
`curve_point_edit.rs:743-749`). **Unification is precisely what makes the deferred keymap work tractable.**

**Owner:** this belongs to the **keymap architecture design already in flight**, intended home
`docs/planning/input/INPUT_ARCHITECTURE_PLAN.md`. ⚠️ **That directory does not exist yet** (verified) — the
plan is in flight, not on disk. **Do not solve the binding here; do not let its absence block S0–S4.**

---

## §9. POLY-ONLY CAPABILITY SURVIVAL CHECKLIST (Feature Dependency Audit — CLAUDE.md, non-negotiable)

| # | Capability | Survives? | Plan |
|---|-----------|-----------|------|
| 1 | **Proportional edit (falloff)** | ✅ **YES — no API change** | `poly_drag_tools.rs:685-707` re-uses the SAME primary transform per-vertex, LERP-weighted — it does **NOT** re-run screen math per vert. It needs a **composable transform** (delta / quat / factor), which the identity-originals trick yields (§6 S3). Keep `FalloffKind` + `all_vert_originals`. **Delete** poly's manual axis-mask (`:675-680`, `:697-702`). Re-key the radius scroll (`input_viewport.rs:257-264`). ⚠️ **note:** that scroll writes **both** `self.poly_prop_radius` **and** `ts.falloff`'s radius — **two sources of truth**; flagged, not fixed here. |
| 2 | **Mirror symmetry** | ⚠️ **YES — but CO-DEPENDENT on D6** | `:708-727` consumes only `new_pos − src_orig` then reflects — **pure delta ⇒ composes**; keep verbatim as the **last step** of the poly branch (it reads *result positions*, so it works for G/R/S uniformly). **BUT the plane is WORLD** (`types_mirror.rs:143`) and the verts are **LOCAL** (`:563-564`, `:710`, `:723`). **Fix in the SAME slice as D6 (S1) or symmetry silently skews on any rotated body.** |
| 3 | **Cancel / vert restore** | ❌ **NO — must be ported, S3** | **Silent corruption, Ladder #1/#3.** See §7. |
| 4 | **Live re-tessellation** | ⚠️ **Only with an explicit early return** | `poly_sync_mesh_cache` + `RENDER` (`:731-732`); `mesh_sync.rs:27-49` is the ONLY thing that re-tessellates + re-uploads. **Disjoint from SDF's `prim_buf_stale`** (`gizmo.rs:1568`). Early-return like the light branch (`:1455`). Same for `finish_gizmo_drag` vs `rebake_no_undo` (`:1598`). |
| 5 | **Numeric input + mid-drag X/Y/Z** | ⚠️ **Only if HOISTED, not deleted** | `input_keyboard.rs:420-443` → variant-agnostic block on `GizmoInteraction`. **Needs `axis_override`** (D9) — the one structural add. |
| 6 | **HUD** | ❌ **NO — must be RE-KEYED (D10 inverted)** | Poly HAS one; `ActiveTool::Gizmo` has **zero** (`grep -c` = 0). **Regression risk.** |
| 7 | **`free_mode`** | ⚠️ **MIGRATE, do not delete (D3)** | Live in the draw (`draw_gizmo.rs:276`/`:443`). |
| 8 | **Ctrl-snap** | ➕ **GAINED — new behavior (D2)** | `snap_active` + `snap_grid` live; `snap_pos: None` for sub-element. |
| 9 | **Elongate** | ❌ **CANNOT survive — reject (D7)** | Blocking. |
| 10 | **Alt+LMB free-move** | ➕ **GAINED — lift the gate** | Align `free_mode` across both entries (GATE). |
| 11 | **Dual pivot** | ✅ **substance KEPT, mechanism CHANGED (D11)** | ONE resolver: `selection_median()`. §7. |
| 12 | **Uniform scale correctness** | ⚠️ **CORRECT TODAY — do not regress** | The **only** op immune to D6 (§4). **Regression check required.** |

**LANDMINE (preserve, do NOT tidy):** `poly_start_transform` calls `begin_episode` (`poly_drag_tools.rs:510`)
**BEFORE** `start_operation` (`:588`), and `start_operation` (`tool_mode.rs:101-112`) `debug_assert`s idle and
**CONFIRMS** (not cancels) an in-flight op. **A leaked-episode ordering exists. It is unchanged by the
migration. Do not reorder it.**

---

## §10. REJECTED ALTERNATIVES

1. **"Unify first, then flip the rotation sign once."** — **REJECTED (§3).** Ships a commit where poly is
   strictly worse (CLAUDE.md: never leave the gap) **and destroys the only reference the owner can judge the
   new sign against.** S0 costs a handful of lines.
2. **"D6 first is wasted — `update_gizmo_drag` already does local↔world."** — **REJECTED, premise false
   (§6 S1).** Those lines are in `start_modal_transform`/`cancel_modal_transform` (the fossil);
   `update_gizmo_drag:1396-1570` has **zero** conversion, and `apply_to_sdf_prim:59`'s raw rot/scale write
   **cannot** serve poly. **The conversion moves; it is never duplicated.**
3. **"Fix D6 by converting the pivot to world."** — **REJECTED (§4).** Fixes REPRO A, leaves **B/C broken**;
   their error is `A·d` on the delta, not the pivot.
4. **"Keep poly's hand-rolled centroid (D11 as written)."** — **REJECTED (§7).** Duplicates
   `poly_selection_center()`, **IS the D6 root**, and locks poly out of `PivotMode` permanently.
5. **"Add a poly HUD (D10 as written)."** — **REJECTED (§2).** Poly has one; the gizmo does not. Inverted.
6. **"Delete `free_mode` (D3 as written)."** — **REJECTED (§2).** Live in the draw layer.
7. **"S5: level SDF up as its own slice."** — **REJECTED (§5).** Emergent from S3; framing it as work is what
   created the rule conflict.
8. **"Land the cancel-restore BEFORE the unify commit."** — **REJECTED (§7).** Unreachable by construction =
   hand-building the fossil. **Same commit.**
9. **"Alias Elongate → Scale on poly (status quo)."** — **REJECTED (§2 D7).** Wrong today; silently
   translates verts post-migration.
10. **"Design the Alt+Ctrl/Alt+Shift normal-drag binding now."** — **REJECTED (§8).** Four divergent
    implementations today, one after S3.
11. **"Match Blender's rotation sign exactly (negate all three)."** — **REJECTED (§3).** Uniform either way,
    but RH+ is the **behavior-neutral** migration for poly and flips only SDF's Z; Blender's negation is an
    **operator-arg** convention, and its modal drag applies a view flip on top.

---

## §11. CITATION CORRECTIONS (so the implementer does not chase ghosts)

Both upstream agents made cite errors. **None are load-bearing; all are corrected here.** The last two are
cases where the **challenge overreached** — recorded because a false correction is as costly as a miss.

1. ✅ **`gizmo.rs:183-185` "the else branch of `apply_transform_result`"** — **there is no
   `apply_transform_result`**; the fn is **`apply_to_scene_node`** (`gizmo.rs:69`). The **claim is true**;
   the correct cite is **`gizmo.rs:1256-1258`** (the non-SdfBody branch of `start_gizmo_drag`).
2. ✅ **`ui_panels.rs:467` as an `is_poly_transform` site** — **false positive** from a broad multi-pattern
   grep; a direct grep of that file returns **no matches**.
3. ❌ **Challenge: *"`types_transform.rs:173` — NO SUCH FILE."*** — **THE CHALLENGE IS WRONG. The file
   EXISTS** at **`core_engine/app_types/src/types_transform.rs`**, and **`:173` is exactly
   `pub free_mode: bool`** on `PolyTransformState` (`:154`). **The facts' cite was RIGHT.** The correct split:
   **`PolyTransformState` lives in `types_transform.rs`; the `ActiveTool` enum + its accessors live in
   `types_tool.rs`.** ⚠️ **Both are in the `app_types` CRATE, not `app/src`** — the challenge's *operational*
   point (S3 is a cross-crate edit) **stands and is important**; only its "no such file" rebuttal was wrong.
4. ❌ **Site count: both agents said "~21 across 8 files."** **Verified: 24 sites across 10 files** — and
   **BOTH missed `viewport_hints.rs:127`**. The S3 deletion surface is **larger than either agent reported**.
5. ⚠️ **TANGENT — pre-existing, do NOT fix opportunistically:** `properties_poly.rs:151-157` writes
   `local_position`/`rotation`/`scale` with only `mark_dirty(DirtyFlags::RENDER)` and **no transaction** —
   violating CLAUDE.md *"Every user-visible slider/toggle in properties panels MUST have undo. No
   exceptions."* It is the **same writer that makes D6 live** and is **step 2 of REPRO A**. **File
   separately.**

---

## §12. SESSION LOG (append newest at top)

- **2026-07-15 — DECISION RECORD CREATED. D1–D12 + gate settled; 4 decisions CHANGED, S5 deleted, 1 rule
  conflict surfaced.** Owner ruled *"THEY SHOULD BE THE SAME"* ⇒ SDF is reference, poly migrates onto
  `compute_transform`. **CHANGED vs the proposal: D4 REJECTED** (plane-exclude doesn't exist for R/S in
  *either* pillar — `transform.rs:175-177`/`:191-193` alias `Plane*`→axis; migration cannot fix it);
  **D10 INVERTED** (poly HAS a HUD, `ActiveTool::Gizmo` has none — `grep -c` = 0 — so the migration is a
  regression risk, re-key not add); **D11 CHANGED** (`PivotMode` **already ships**, `selection_median()`
  already resolves it and already has the poly branch, the gizmo is drawn at it — and the math discards it:
  `pivot` = 0 hits in `transform.rs`/`poly_drag_tools.rs`, `node.pivot` = 0 in `gizmo.rs`; poly's hand-rolled
  centroid is DELETED, and the **SDF-side pivot defect is a separate filed bug**); **D5 ORDER FLIPPED**
  (Blender oracle: `transform.rotate` negates a positive value vs the RH rule on **all three axes
  identically** — AZRA's Z-only `-angle` is **ad-hoc**, and `perpendicular_axis()`'s `.abs()` makes SDF
  **view-blind**; **poly is the more correct pillar on both counts**, so `compute_transform` is fixed in
  **S0, BEFORE** unifying — else the commit ships poly strictly worse and destroys the owner's only A/B
  reference). **D3 half-false** (`free_mode` dead in math, live in `draw_gizmo.rs:276`/`:443` — migrate);
  **D7 promoted to BLOCKING**; **D9 underscoped** (needs `axis_override` — the only structural add).
  **RULE CONFLICT (§5):** "Architecture task = refactor only, no new features" vs D8/D9 — **resolves in favor
  of proceeding** (the capabilities are **emergent**: suppressing them for SDF would need an extra guard
  re-creating the fossil), **S5 DELETED** → S3 acceptance criterion. **§7 HARD CONSTRAINT:** S3 must **delete
  `ActiveTool::PolyTransform` + accessors** (cross-crate, `app_types`) — the `_ => None` at
  `types_tool.rs:280` is what fossilized the **last** run of this exact migration
  (`input_viewport.rs:1278-1281` documents it; `start_modal_transform`'s only caller is now
  `input_keyboard.rs:943` = lights, leaving ~150 unreachable lines at `gizmo.rs:619-771`). Verified **24
  sites / 10 files** (both agents said ~21/8; **both missed `viewport_hints.rs:127`**). **Cancel gap
  CONFIRMED** (`gizmo.rs:1601-1655` has no poly branch → `:1654` rolls back the episode while verts stay
  moved = Esc silently corrupts geometry) — lands **IN S3, same commit**. Final order: **S0 sign/view → S1
  D6+mirror+reuse → S2 D7 → S3 UNIFY → S4 snap/free_mode/gate.** Read-only session; no source touched.

---

## §13. GRAVEYARD — transform-parity dead-ends (never retry)

- **NEVER add an `Option`/bool accessor (`as_*()`/`is_*()`) as the ONLY consumer contract for an
  `ActiveTool` variant you intend to migrate off.** `types_tool.rs:280`'s `_ => None` let the **previous**
  run of this exact migration (`input_viewport.rs:1278-1281`) strand ~150 lines at `gizmo.rs:619-771` with a
  **fully green build** and no warning. **Lesson: delete the variant so the compiler enumerates the surface
  — a red build is a SAFE stall; a silently-dead accessor is the fossil.** *(2026-07-15)*
- **NEVER hardcode a field to `false` in a struct literal to "get the migration compiling."**
  `poly_drag_tools.rs:630` (`snap_active: false, free_mode: false`) is the fossil of the 25%-complete
  migration — it looked like a decision and was actually a stall, and it silently killed two features for
  months. **Lesson: if you cannot plumb it honestly, don't construct the call yet.** *(2026-07-15)*
- **NEVER assume "unify onto the shared path" means the shared path is CORRECT.** SDF's rotate is
  view-blind (`perpendicular_axis()`'s `.abs()`, `types_gizmo.rs:91-93`) and sign-asymmetric (Z-only
  `-angle`, `transform.rs:177`), while poly is view-aware and uniform. **The reference direction is
  per-convention, not per-pillar.** *(2026-07-15)*
- **NEVER trust an ad-hoc sign flip as a "handedness correction."** AZRA negates **only Z**; the Blender
  oracle negates **all three identically**. **A genuine handedness fix is uniform across axes — an
  asymmetric one is a symptom-patch on one view.** *(2026-07-15)*
- **NEVER cite a `pivot`/`median` variable as proof the math honors it.** `median_start_pos` **is** captured
  into every `GizmoInteraction` (`gizmo.rs:1199`…) and is then applied **only `if multi_body`** (`:1475`),
  while the prim branch hardcodes the SdfBody `origin` (`:1498-1505`). **Lesson: trace the value to where it
  is USED (POLY_POLISH_PLAN §8's LoopTools lesson, same class) — capture is not consumption.**
  *(2026-07-15)*
- **NEVER "fix D6" by world-ifying the pivot alone.** It fixes the plain-G overshoot (REPRO A) and leaves
  the rotation-axis (B) and scale-speed (C) errors, because those live in `A·d` on the **delta**.
  **Lesson: run the math in world end-to-end, map back with `M⁻¹`.** *(2026-07-15)*
- **NEVER let a "background-mode Blender op ignored the setting" result read as "the feature doesn't
  exist."** P6 showed `bpy.ops.transform.*` ignoring `transform_pivot_point` under `--background`; **P7
  (`center_override`) proved the operator supports an arbitrary pivot.** **Lesson: `bpy.ops` in bg mode is
  not a faithful oracle for MODAL/tool-settings behavior — property reads (P1) are; separate the two before
  concluding.** *(2026-07-15)*
</content>
</invoke>
