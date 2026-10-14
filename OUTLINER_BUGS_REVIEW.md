# Outliner subsystem — review findings (queued AFTER Brush Phase 0)

Captured 2026-06-29. Owner directive: tackle these **after** Brush Phase 0 lands. Full detail in the owner's
review message; this is the actionable triage + the batching plan (the outliner files are a contention point,
like the sculpt files, so most of these serialize by file-group).

## 🔴 Critical — crashes / data corruption
1. **Stale `cached_body_buckets` → OOB panic + silent wrong-row mutation** (3 reviewers, highest confidence).
   `outliner_draw_body.rs:337-341/:375`, throttle gate `outliner_draw.rs:91-97`. Bucket invalidation is gated
   behind the throttle; during interaction (orbit/sculpt/tool, ≥2 frames) a structural mutation (Ctrl+Z a prim
   create, Delete) shrinks `scene.primitives` while buckets hold stale indices → next draw indexes OOB → panic;
   on length-unchanged reorder it toggles the WRONG prim silently. **Fix structurally: key buckets by `prim_uid`
   + resolve via `prim_index_by_uid()` at draw time, OR invalidate on `outliner_dirty`/`sync_flat_cache`** (NOT a
   bounds guard — that hides the silent wrong-prim case). Also resolves #9's class.
2. **Multi-prim cross-body reparent uses stale flat indices across `sync_flat_cache`** · high.
   `outliner_dnd.rs:259-262` (+`:282-285`), `prim_actions.rs:189-214`. Each `reparent_prim_to_body` calls
   `sync_flat_cache()`, shifting remaining captured indices by +1 (insert into earlier-DFS target) → 2nd prim
   wrong / dropped. **Fix: resolve uids up front, re-resolve via `prim_index_by_uid()` inside the loop** (the
   same-body reorder path already does this).
3. ~~**Reparenting a body leaves CR-74 ghost volume in the OLD parent**~~ · **FIXED 2026-07** —
   `reparent_body_node` (`scene_actions.rs`) marks old parent + new parent + child via
   `mark_node_body_vol_dirty` after `ensure_body_child_order` on both parents. Nest/un-nest
   (keyboard + outliner NestInto) share this single path.
4. **SDF→Poly convert skips the structural checklist** · medium.
   `convert.rs:46-57` (from `outliner_draw_body.rs:201`). Overwrites payload but never removes prims from the flat
   cache / `sync_flat_cache` / `body_vol_cache.remove_body` → ghost prims + leaked grid. **Mirror the delete path**
   (`remove_prims_from_graph` + `remove_body`).

## 🟠 High / medium
5. **Body drag-reorder lands one slot too far dragging downward** · high (2×). `outliner_dnd.rs:133-137`,
   `reorder_child` `scene_graph.rs:1256-1264`. insert_idx computed with the dragged body still present, but
   reorder_child removes it first → overshoot. **Fix in reorder_child: `if pos < target_index { target_index-1 }`**
   (prim path already compensates). Sub-body order = boolean eval chain → wrong silhouette bakes.
6. **Drag-to-group creates TWO undo entries (+ phantom empty)** · high (2×). `outliner_dnd.rs:184-191`,
   `prim_actions.rs:402`. Outer `transaction_snapshot_only` + group's own `transaction()` = 2 Ctrl+Z; cross-body
   drop strands a no-op "Group prims" entry. **Fix: drop the outer snapshot; fold into group's single txn.**
7. **Shift-range-select no-ops on the clicked row (all row types)** · medium. `outliner_draw_light.rs:101-103`
   etc. `visual_order` lacks the target row at shift-click time (pushed after the handler) → selects nothing.
   **Fix: defer range resolution to `process_outliner_deferred`, or build `visual_order` in a pre-pass.**
8. **`frame_selected()` mixes body-local prim pos with world-space center** · medium. `outliner_actions.rs:125-135`.
   center is world (local_to_world) but radius loop uses raw body-local `p.position` → F-key zoom wrong for
   off-origin bodies. **Fix: `bt.local_to_world(p.position)`, gated on BODY_LOCAL_PRIMS** (Prim Coordinate Space rule).

## 🟡 Low — latent / cosmetic / cleanup
9 rename defers flat index not uid (`outliner.rs:143`) — folds into #1's fix · 10 rename allows whitespace-only
name (no `trim()`, `outliner_actions.rs:40`) · 11 direct `usel.selected.insert()` bypasses selection writers
(`outliner_dnd.rs:184`) · 12 two divergent scope-enter paths, no canonical `enter_body_scope()`
(`outliner_draw_body.rs:172` vs `input_viewport.rs:1424`) · 13 name-only rename schedules an unnecessary GPU bake
(`prim_actions.rs:357-376`) · ~~14 `reparent_body` dead code~~ **FIXED/REMOVED** — nest is inline in
`outliner_dnd.rs` via `reparent_body_node`; deferred field gone from `outliner_panel.rs`.
*(Rejected: the `toggle_select_unified` IndexSet::last() "wrong active" claim — verifier showed active∈selected is
preserved, arbitrary-but-valid.)*

---

## ✅ 2026-07-21 kitbash Group multi-agent review (appended)

See `COMPLETE_BUG_TRACKER.md` section **2026-07-21 (Kitbash Group + multi-body outliner DnD)**.

Shipped: body drag into Group, RMB Create Group 1+, shared drop helper, Group/OBJ drag payload,
empty-area un-nest for Group children, forbid Group-under-SdfBody, common-parent Create Group.

## Batching plan (after Phase 0; outliner files serialize by group)
- **Agent A** — #1 (uid-key buckets / invalidate on outliner_dirty) + #9 + #10 → `outliner_draw*.rs`, `outliner.rs`,
  `outliner_actions.rs`. The keystone (kills the index-staleness class).
- **Agent B** — #2 + #5 + #6 + #11 → `outliner_dnd.rs` + `prim_actions.rs` + `scene_graph.rs::reorder_child`
  (the reparent/reorder/group index+undo fixes).
- **Agent C** — #3 + #14 → decide wire-vs-delete body nesting (`outliner_panel.rs`, `scene_graph.rs`).
- **Agent D** — #4 (`convert.rs`) + #8 (`outliner_actions.rs` frame) + #7 (shift-range, multi-file) + #12/#13.
Recommended order (owner): #1 → #2+#5 → #6+#7 → #3+#14.
