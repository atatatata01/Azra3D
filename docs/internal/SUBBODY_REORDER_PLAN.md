# SUBBODY_REORDER_PLAN.md — Tier A "Unified Child Ordering"

**Status:** DESIGN ONLY. Do NOT implement without owner sign-off — this is a **save-format change** (bincode + serde) and touches the SDF eval/bake order. Implementing it wrong corrupts every existing scene file.

**Author context:** Written alongside the Part A fix (body-reorder thin-slab bake bug). The Part A lesson — *every reorder entry point must call `mark_node_body_vol_dirty`* — is a hard requirement of this design (see §8).

---

## 1. The Problem

Inside one SDF body, the owner cannot freely reorder a sub-body / `Group` *among* loose prims. The two kinds of children live in **two separate, non-interleavable orderings**:

- **Loose prims** live in `SdfBody.primitives: Vec<SdfPrimitive>` (`scene_graph.rs:268`).
- **Sub-bodies / child nodes** live in `SceneNode.children: Vec<NodeId>` (graph edges).

Every consumer hard-codes the convention **"all child bodies, then all loose prims"** (or the reverse). There is no single sequence the user can rearrange:

| Consumer | File:line | Current hard-coded order |
|---|---|---|
| Outliner draw | `outliner_draw_body.rs:321-341` | child bodies (322-334) **then** flat prims (336-341) |
| Eval / bake serializer | `graphics/src/serialize.rs:338-362` | prims (338-355) **then** child bodies (357-362) |
| DnD prim reorder | `outliner_dnd.rs:168-239` | reorders within `primitives` only |
| DnD body reorder | `outliner_dnd.rs:113-153` | reorders within `children` only |
| Keyboard/scroll reorder | `hierarchy_order.rs` | `reorder_selected_prims` (prims) vs `reorder_selected_nodes` (children) — two disjoint code paths |

Note the draw loop and the bake serializer even **disagree** on which group comes first (bodies-first vs prims-first). That is invisible today only because boolean union is commutative for the *display grouping*, but it proves there is no canonical sequence to expose to the user.

The SDF boolean chain **is order-dependent** (subtract / intersect after a base shape). So "where a sub-body sits relative to the loose prims" is semantically meaningful, and the user currently cannot express it.

---

## 2. The Fix — One Ordering, Two Storage Owners

Introduce a per-body **`child_order: Vec<BodyChildRef>`** that defines the ONE display + eval sequence. It does **not** own data — it is an ordering index. The two existing Vecs remain the **storage owners**:

- `SdfBody.primitives` still OWNS prim data (`SdfPrimitive` values).
- `SceneNode.children` still OWNS the graph edges (parent/child topology, transforms, selection by `NodeId`).

`child_order` is a third list whose every element points (by stable ID) into one of those two owners. This respects **Scene-Graph-Is-The-Only-Owner**: no data is duplicated, only *referenced* by stable ID for sequencing.

```
SdfBody
├── primitives: Vec<SdfPrimitive>      // OWNS prim data        (unchanged)
├── (SceneNode.children: Vec<NodeId>)  // OWNS child topology   (unchanged)
└── child_order: Vec<BodyChildRef>     // NEW: the one sequence (references only)
```

### Invariant (must hold after every mutation)
`child_order` is a permutation of `{ Prim(uid) for every prim in primitives } ∪ { Body(nid) for every SdfBody/Group child in children }`. No missing refs, no dangling refs, no duplicates. A debug-only `validate_child_order(body)` asserts this after each mutating op (assert, never silently repair — repairing would hide the bug per CLAUDE.md No-Bandaids).

---

## 3. Data Model — `BodyChildRef` (scene_graph.rs)

`scene_graph.rs` is **not** a frozen file, so this is permitted.

```rust
/// One slot in a body's unified child ordering. References (never owns) either a
/// loose prim (by stable PrimUid) or a child node (by NodeId). The pointed-to data
/// still lives in SdfBody.primitives / SceneNode.children respectively.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyChildRef {
    Prim(u32),   // PrimUid — resolved via prim_index_by_uid at use time
    Body(NodeId),
}
```

### `SdfBody` field addition (`scene_graph.rs:265-306`)

Append the field at the **end** of the `SdfBody` struct body, with `#[serde(default)]`:

```rust
SdfBody {
    name: String,
    primitives: Vec<SdfPrimitive>,
    // ... all existing fields unchanged ...
    material: PbrMaterial,
    /// Unified display+eval ordering over this body's loose prims AND child bodies.
    /// References only (PrimUid / NodeId); primitives + SceneNode.children remain the
    /// data owners. Empty on load from a pre-v8 file → synthesized by migration (§7).
    #[serde(default)]
    child_order: Vec<BodyChildRef>,
}
```

### Bincode safety (CLAUDE.md GPU Safety / serialized enums)

- **`BodyChildRef` is a NEW enum.** It is self-contained — its variants (`Prim`, `Body`) are append-safe because the enum is new; nothing reads an older tag layout. Once shipped, treat it append-only (never insert/reorder variants).
- **`SdfBody` is a struct, not a bincode-tagged enum**, so adding a field is a **struct-layout** change, not a variant-index change. With `bincode` + a fixed field order, old files (without the field) will NOT deserialize a struct that now expects an extra trailing field — **plain bincode is not self-describing** and cannot skip a missing trailing field, so `#[serde(default)]` alone does NOT rescue old files in bincode.
  - **Therefore the migration is version-gated (§7), not serde-default-driven.** We bump the file version to **v8** and run an explicit `migrate_v7_to_v8` that *constructs* `child_order` for every body when loading a v7-or-older file. The `#[serde(default)]` is belt-and-suspenders for the in-memory default and for any self-describing debug dumps; correctness rests on the version gate.
  - **Do NOT reorder or insert any field above `child_order`** in `SdfBody` (bincode reads struct fields positionally). Append only.

`NodePayload::VoxelBody` / `SdfObject` remain the last enum variants — this change does not touch `NodePayload`'s variant list, so that constraint is untouched.

---

## 4. Outliner Draw Loop (outliner_draw_body.rs:321-341)

Replace the two sequential loops (child-bodies-then-prims) with **one loop over `child_order`**:

```rust
// BEFORE: two loops, fixed grouping
//   for cb_nid in child_bodies { draw_outliner_sdf_body(..) }   // 322-334
//   for &i in &flat { draw_prim_row_clipped(..) }               // 336-341

// AFTER: one loop, user-defined sequence
let order = self.scene.scene_graph.body_child_order(child_nid); // clone Vec<BodyChildRef>
for slot in order {
    match slot {
        BodyChildRef::Body(cb_nid) => {
            // visibility + name fetch, then recurse (existing 324-333 block)
            self.draw_outliner_sdf_body(ui, ctx, cb_nid, &cb_name, cb_visible, /* ... */, depth + 1);
        }
        BodyChildRef::Prim(uid) => {
            let i = match self.scene.prim_index_by_uid(uid) { Some(i) => i, None => continue };
            if !prim_matches(i, &self.scene.primitives) { continue; }
            self.draw_prim_row_clipped(ui, ctx, i, ctrl, shift, vis_ch, d, &clip);
        }
    }
}
```

- The cached `BodyPrimBuckets.flat` (built in `build_body_prim_buckets`, `outliner_draw_body.rs:346-361`) is no longer the iteration driver; it can stay as a membership/filter cache but the **sequence comes from `child_order`**.
- `child_bodies()` recursion through `SdfObject` wrappers (`scene_graph.rs:1122`) still applies for *display nesting*, but a `Group`/`SdfObject` child that sits in `child_order` is one `Body(nid)` slot at this level.

---

## 5. DnD Unification — One Drop-Line (outliner.rs / outliner_dnd.rs)

Today there are **two** drag systems with separate drop targets:

- prim drag: `dragging`, `drop_target`, `drop_target_y`, `drop_before` (`outliner_dnd.rs:156-239`)
- body drag: `dragging_body`, `body_drop_target_nid`, `body_drop_target_y`, `body_drop_before` (`outliner_dnd.rs:113-153`)

**Unify into one drag concept keyed on `BodyChildRef`:**

- One in-flight drag payload: `Option<BodyChildRef>` (what is being dragged).
- One drop target: an **insert position within the target body's `child_order`** — `(target_body: NodeId, insert_index: usize)`, plus the existing y-coordinate for the drop-line painter (reuse `drop_target_y` / `drop_before`).
- The drop-line is computed once: each rendered row (whether prim or sub-body) reports its rect; the insert index is "before row k" or "after last row", exactly like the current prim path, but over the merged row list (which is already merged because §4 draws from one `child_order`).

### Release handler (replaces both blocks)
```rust
self.transaction("Reorder child", |s| {
    // 1. Mutate child_order: remove dragged ref, insert at insert_index.
    s.scene.scene_graph.reorder_child_order(target_body, dragged_ref, insert_index);
    // 2. If dragged_ref is Body(nid) and target_body differs from its current
    //    parent body → this is a reparent: also move the graph edge
    //    (reorder_child / reparent), and for Prim(uid) crossing bodies route to
    //    the EXISTING reparent_prim_to_body path. Same-body = pure reorder.
    s.recalculate_body_origin(target_body);   // checklist step 2 (membership may change on reparent)
    s.sync_flat_cache();                       // checklist step 3
    s.mark_node_body_vol_dirty(target_body);   // checklist step 4 — see §8
    // also mark the SOURCE body if this was a cross-body move
    s.mark_dirty(DirtyFlags::SCENE);
    s.mark_outliner_dirty();
});
```

- **Same-body reorder** is the common case (the feature the owner wants): only `child_order` changes; `primitives` and `children` are untouched as *sets*.
- **Cross-body drag** (reparent) keeps routing prim moves through the existing `reparent_prim_to_body` and body moves through `reorder_child` + edge reparent — but now the destination position is an index into the target's `child_order`, and on insert we add the new `BodyChildRef` at that slot (and remove it from the source body's `child_order`).

---

## 6. Eval / Bake Serializer (graphics/src/serialize.rs:335-362)

The bake serializer currently emits **all prims, then recurses into all child bodies**. Replace with **one pass over `child_order`** so the GPU boolean chain matches the outliner:

```rust
// BEFORE (335-362): prims loop, then child_bodies loop.
// AFTER: single ordered pass.
for slot in sg.body_child_order(body_id) {
    match slot {
        BodyChildRef::Prim(uid) => {
            // resolve uid -> &SdfPrimitive within this body's `primitives`
            let (i, p) = match resolve_prim_in_body(prims, uid) { Some(x) => x, None => continue };
            if !p.visible { continue; }
            // ... existing BODY_LOCAL_PRIMS branch + Lipschitz hot[74] write (340-354) ...
        }
        BodyChildRef::Body(child_id) => {
            if let Some(cn) = sg.get(child_id) { if !cn.visible { continue; } }
            serialize_body_for_bake_hot_recursive(sg, child_id, false, empty_sel, origin_offset, out);
        }
    }
}
```

- Mirror the SAME change in the non-`_hot` twin `serialize_body_for_bake_recursive` (`serialize.rs:369+`) so both code paths stay in parity (CLAUDE.md: two paths for one op must share semantics — ideally factor the ordered-walk into a shared helper that yields `BodyChildRef`s, so the two serializers cannot diverge).
- `scene_analytical()` / GLSL BVH / WGSL analytical paths that walk a body's prims must consume the same order. Because analytical is DISABLED by default (volume-only mainline), the **bake serializer is the load-bearing one**; analytical parity is a follow-up in the same task IF analytical is ever re-enabled (note it, don't skip silently — CLAUDE.md "Feature Must Work in All Three Paths").
- **Resolution by `prim_index_by_uid` happens at serialize time only** (CLAUDE.md Stable-IDs rule) — `child_order` stores `PrimUid`, never a flat/local index.

---

## 7. Save-Format Bump + Migration (project_load.rs)

- Bump `CURRENT_SAVE_VERSION` **7 → 8** (`project_load.rs:42`).
- Add `migrate_v7_to_v8(sg)` following the exact shape of `migrate_v4_to_v5` (`project_load.rs:12-39`): collect all `SdfBody` NodeIds, then for each body **synthesize `child_order`** if empty.

### Synthesized order = today's effective order, so NO visual change for old files

The current *bake* order is **prims first, then child bodies** (`serialize.rs`). To guarantee a loaded v7 file renders identically, migration must reproduce that:

```rust
fn migrate_v7_to_v8(sg: &mut SceneGraph) {
    let body_nids: Vec<NodeId> = sg.all_nodes()
        .filter_map(|(nid, n)| matches!(&n.payload, NodePayload::SdfBody { .. }).then_some(nid))
        .collect();
    for nid in body_nids {
        // child SdfBody/Group nids, in current children order
        let child_bodies = sg.child_bodies(nid); // matches draw/eval child-body semantics
        let prim_uids: Vec<u32> = match sg.get(nid).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { primitives, child_order, .. }) => {
                if !child_order.is_empty() { continue; } // already v8+, leave as-is
                primitives.iter().map(|p| p.prim_uid).collect()
            }
            _ => continue,
        };
        let mut order = Vec::with_capacity(prim_uids.len() + child_bodies.len());
        // MATCH CURRENT BAKE ORDER: prims first, then child bodies.
        order.extend(prim_uids.into_iter().map(BodyChildRef::Prim));
        order.extend(child_bodies.into_iter().map(BodyChildRef::Body));
        if let Some(NodePayload::SdfBody { child_order, .. }) = sg.get_mut(nid).map(|n| &mut n.payload) {
            *child_order = order;
        }
    }
}
```

- **Decision to confirm with owner:** the outliner currently *draws* bodies-then-prims while the bake does prims-then-bodies. Migration must match the **bake** order (that is what the surface looks like today). The outliner will visually re-order to match after v8 — a cosmetic shift in the tree, **not** a surface change. Flag this explicitly at sign-off.
- Call site: in `apply_to_app`, alongside the existing version-gated migrations (`project_load.rs:209-211` pattern): `if self.version <= 7 { migrate_v7_to_v8(&mut app.scene.scene_graph); }`.
- **v8 save** writes `child_order` directly; round-trips with no migration.
- Keep `child_order` consistent on EVERY structural mutation that adds/removes a prim or child (create, delete, reparent, group, dissolve, duplicate). Each of those must append/remove the matching `BodyChildRef`. A central `body_child_order_mut(nid)` accessor + helpers (`push_prim_ref`, `push_body_ref`, `remove_ref`) keeps this in one place. Wire these into the existing `create_prim`, `delete_prim`, `reparent_prim_to_body`, `group_selected_into_body`, `duplicate_subtree` paths — the **Structural Mutation Checklist** gains a step 0: "update `child_order`".

---

## 8. Reorder Entry Points Collapse Onto One `child_order` Reorder

After this change there is exactly **one** reorder operation: *move a `BodyChildRef` to a new index within a body's `child_order`.* All current entry points route to it:

| Old path | New behavior |
|---|---|
| `reorder_selected_prims` (`hierarchy_order.rs:53`) | builds `BodyChildRef::Prim(uid)` list → `reorder_child_order` |
| `reorder_selected_nodes` (`hierarchy_order.rs:110`) | builds `BodyChildRef::Body(nid)` list → `reorder_child_order` |
| DnD prim reorder (`outliner_dnd.rs:168-239`) | one drop-line → `reorder_child_order` (§5) |
| DnD body reorder (`outliner_dnd.rs:113-153`) | one drop-line → `reorder_child_order` (§5) |
| `reorder_prim_in_graph` (`prim_actions.rs:156`) | thin shim → `reorder_child_order` |

**Every one of these MUST call `mark_node_body_vol_dirty(body)` + `mark_dirty(DirtyFlags::SCENE)`** after mutating `child_order` — this is the **Part A lesson** generalized. Reordering changes the boolean eval chain → structural → the body's baked volume is stale until re-baked. `SCENE` alone does NOT bake (`process_dirty` skips `trigger_body_vol_bake` when no body is in `dirty_bodies`); the explicit `mark_node_body_vol_dirty` is mandatory (Structural Mutation Checklist step 4). Centralizing into ONE `reorder_child_order` makes this a single, un-bypassable site — which is the structural way to prevent the thin-slab class of bug from ever recurring (No-Bandaids test #1 + #3).

`reorder_child_order` itself is wrapped by callers in `transaction()` (atomic) or an episode (DnD drag). The accessor mutates the graph only; undo is captured by the surrounding transaction exactly like every other mutation.

---

## 9. New `SceneGraph` API (scene_graph.rs)

```rust
impl SceneGraph {
    /// Read the unified child ordering of a body (clone — callers iterate then drop the borrow).
    pub fn body_child_order(&self, body_id: NodeId) -> Vec<BodyChildRef>;
    /// Mutable access for the centralized reorder/structural helpers.
    pub fn body_child_order_mut(&mut self, body_id: NodeId) -> Option<&mut Vec<BodyChildRef>>;
    /// Move `child` to `insert_index` within body's child_order (the ONE reorder op).
    pub fn reorder_child_order(&mut self, body_id: NodeId, child: BodyChildRef, insert_index: usize);
    /// Debug-only: assert child_order is a permutation of {prims} ∪ {child bodies}.
    #[cfg(debug_assertions)]
    pub fn validate_child_order(&self, body_id: NodeId);
}
```

These live next to `reorder_child` (`scene_graph.rs:1237`) and `child_bodies` (`scene_graph.rs:1117`). **No duplication:** `reorder_child` (node edges) stays as the topology op; `reorder_child_order` is purely the sequence index. The DnD reparent case calls both.

---

## 10. Scope / Risk / Sign-Off Checklist

- [ ] Owner confirms the **save-format bump to v8** and the migration's "match current BAKE order (prims-then-bodies)" decision (§7), accepting the cosmetic outliner re-ordering of old files.
- [ ] `BodyChildRef` enum is append-only henceforth; `child_order` field is appended LAST in `SdfBody` (bincode positional safety, §3).
- [ ] Every structural mutation (`create_prim`, `delete_prim`, reparent, group, dissolve, duplicate) updates `child_order` — added as step 0 of the Structural Mutation Checklist.
- [ ] Every reorder path calls `mark_node_body_vol_dirty` + `SCENE` (§8) — enforced by funnelling through one `reorder_child_order`.
- [ ] Bake serializer (`serialize.rs`) `_hot` and non-`_hot` twins share ONE ordered-walk helper (no parity drift).
- [ ] Undo: `reorder_child_order` is graph-only; the surrounding `transaction()`/episode owns undo. Save-load round-trips v8 without migration; v7 migrates once.
- [ ] Manual test: a body with `[primA, subBodyX, primB]` — drag `subBodyX` above `primA`; verify outliner shows the new order, the bake re-fires (no thin slab), undo restores, save→reload preserves the order.

---

## 11. Files Touched (implementation, when approved)

| File | Change |
|---|---|
| `core_engine/core_math/src/scene_graph.rs` | `BodyChildRef` enum; `SdfBody.child_order` field; `body_child_order` / `_mut` / `reorder_child_order` / `validate_child_order` API |
| `core_engine/app/src/outliner_draw_body.rs` | one draw loop over `child_order` (§4) |
| `core_engine/app/src/outliner.rs`, `outliner_dnd.rs` | unify drag/drop onto one `BodyChildRef` + one drop-line (§5); collapse reorder release handlers |
| `core_engine/app/src/hierarchy_order.rs` | both reorder fns → `reorder_child_order` |
| `core_engine/app/src/prim_actions.rs` | `reorder_prim_in_graph` → shim; structural ops update `child_order` |
| `core_engine/app/src/scene_actions.rs` | create/delete/reparent/group/dissolve/duplicate update `child_order` |
| `core_engine/graphics/src/serialize.rs` | ordered-walk over `child_order` in both bake serializers (§6) |
| `core_engine/app/src/project_load.rs` | `CURRENT_SAVE_VERSION` 7→8; `migrate_v7_to_v8` (§7) |
| `CLAUDE.md` (docs) | Structural Mutation Checklist gains "step 0: update child_order"; note v8 save format |

**End of design. Implementation gated on owner sign-off (§10).**
