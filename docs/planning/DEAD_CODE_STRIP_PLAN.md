# Dead-Code Strip Plan (Corrected)

**Status:** Owner-approved framework · D1–D4 ship-ready · D5/D7 rules corrected · D6/D8/D9 owner-in-loop  
**Source inventory:** dead-code review session 2026-07-19 (`cargo check` + call-graph)  
**Scope:** `core_engine` (app / graphics / render / core_math)  
**Not in scope:** B1 G-buffer work, feature design, renames for style

---

## 0. Goals & non-goals

| | |
|--|--|
| **Goal** | Remove *statically proven* dead code without behavior change |
| **Success** | Cleaner tree, fewer warnings, smaller PT shader source; app still builds & smokes |
| **Non-goals** | Refactors; “maybe useful later” APIs without product sign-off; deleting live/mid-pipeline surfaces that look unused under a single `cargo check` |

---

## 1. Methodology (owner-approved — non-negotiable)

1. **One slice ID per session** (e.g. `D1`). One slice = one revert.
2. **Grep before delete** — zero call sites, and no string / `include` / shader-assembly tricks.
3. **Gate:** `cargo check -p app -p graphics -p render -p core_math` green after every slice.
4. **WGSL extra gate:** `cargo test -p graphics --lib validate_pt_tri_shader` (naga).
5. **Prefer `#[allow(dead_code)] // <reason>` over deletion** for anything a plan doc mentions, anything mid-pipeline, or anything an owner might still ship.
6. **Owner gates product surface** (clipboard, array-dup, warming, UI remnants).
7. **No drive-by** edits outside the slice’s listed files.
8. **If unsure → skip** and mark `NEEDS-OWNER`. Do not guess product intent.
9. **Agent-finishable slices** are only those verifiable with cargo/naga alone. Launch-app smoke is **owner**, not agent (exe may be locked; headless cannot substitute).

### Enum / save-format rule (applies to every variant delete)

> **Before removing any enum variant:** confirm the enum does **not** derive or implement `Serialize` / `Deserialize` (bincode or serde). Deleting a variant from a serialized enum shifts later tags and **corrupts every `.fl4`**.  
> If the enum is serialized → **tombstone / keep + allow**, never mid-list delete.  
> If not serialized → delete is allowed only after match-arm cleanup.

### GPU dummy / BGL rule (applies to D8 and any buffer delete)

> “Never read in Rust” ≠ dead. Dummy buffers can still be **load-bearing in bind-group layouts**.  
> Before removing any GPU buffer/texture: **grep `*_bgl_entries()`** and every place that builds the matching bind group. If the layout still lists the binding, keep the resource (or shrink the layout in a dedicated, smoke-gated slice).

---

## 2. Phase map (corrected)

```
Phase D0   Prep / baseline warnings              [read-only]
Phase D1   PT WGSL orphans                       [agent OK · ship now]
Phase D2   PT Rust legacy                        [agent OK · ship now]
Phase D3   Unused imports (cargo fix)            [agent OK · ship now]
Phase D4   Unused locals                         [agent OK · ship now]
Phase D5   Dead enum variants (serialize-guard)  [careful · serialize check first]
Phase D6   Warming / clipboard / array-dup       [owner keep/kill · hold for smoke]
Phase D7   Bake / brick / classify / LOD         [KEEP + annotate · NOT strip]
Phase D8   GpuCtx / pipeline leftovers           [owner smoke · BGL grep first]
Phase D9   Outliner deferred + misc UI           [owner smoke · keep-or-wire]
Phase D10  Final audit + warning budget          [owner smoke + cargo]
```

**Ship now (agent-complete):** D1 → D2 → D3 → D4.  
**Hold for owner-in-loop:** D6, D8, D9 (and D10 smoke half).  
**Recast (no delete of live clusters):** D7.

```
D0 ──► D1 ──► D2 ──► D3 ──► D4 ──► D5a/b (serialize-safe only)
                                    │
                    owner gate ─────┼──► D6a/b/c   (keep or strip per cluster)
                                    ├──► D7        (KEEP + #[allow] only)
                                    ├──► D8        (BGL grep + owner smoke)
                                    └──► D9        (owner smoke)
                                              └──► D10
```

---

## 3. Phase D0 — Baseline (½ session, optional)

| | |
|--|--|
| **Work** | Capture current `cargo check` unused/dead_code list. Count warnings. |
| **Files** | None (read-only) |
| **Done when** | Baseline count recorded |
| **Verify** | `cargo check -p app 2>&1` → filter never/unused/dead_code |

---

## 4. Phase D1 — PT WGSL orphans ⭐ start here (agent OK)

Owner-verified: these five are **genuinely dead islands**. Live paths use `_table` / `_candidate` / `pt_restir_gi` variants — **keep those**.

| | |
|--|--|
| **Problem** | Dead helpers left after ReSTIR GI / light-table refactor |
| **Files** | `graphics/src/pathtrace_tri_entry.wgsl` **only** |
| **Delete** | `pt_sample_light_index`, `pt_light_pick_pdf`, `pt_phat_for_light` (non-table), `pt_sample_gi`, `pt_sample_1bounce_gi` |
| **Keep** | `pt_phat_for_light_table`, `pt_restir_gi`, all table/RIS / candidate paths |
| **Pre-delete** | Grep whole `graphics` + `shader_build` for each name |
| **Done when** | Grep shows zero refs to deleted names; naga validate OK |
| **Verify** | `cargo test -p graphics --lib validate_pt_tri_shader` |
| **Risk** | **Low** |

---

## 5. Phase D2 — PT Rust legacy (agent OK)

| | |
|--|--|
| **Files** | `app/src/viewport_render_mode.rs` |
| **Delete** | `PT_MAX_ACCUM_FRAMES`, `pt_target_frames` |
| **Keep** | `PathTraceQuality::lighting_size` if tests use it (or inline in tests then delete — prefer keep for now) |
| **Done when** | No references; tests still pass |
| **Verify** | `cargo test -p app --bin app pt_quality` (or equivalent filter) |
| **Risk** | **Low** |

---

## 6. Phase D3 — Unused imports (agent OK, mechanical)

| | |
|--|--|
| **Work** | Unused-import cleanup only (`cargo fix -p app --bin app --allow-dirty` or file-by-file) |
| **Files** | Many app modules + `core_math` knife if flagged |
| **Out of scope** | Logic changes, re-export “fixes” that change public surface |
| **Done when** | Unused-import warnings largely gone |
| **Verify** | `cargo check -p app` |
| **Risk** | **Very low** (watch false positives on re-exports) |

**Sub-slices if context tight:**  
- **D3a** `main.rs` + poll modules  
- **D3b** properties / menus  
- **D3c** core_math imports  

---

## 7. Phase D4 — Unused locals / assignments (agent OK)

| | |
|--|--|
| **Fix** | Prefix `_` or remove dead assigns |
| **Examples** | `prim_count`, `wg_x`/`wg_y`, `hit_r`, `sel_box_just_finished`, `tex3d_entry`, … |
| **Files** | Compiler-driven, file-by-file |
| **Sub-slices** | D4a graphics serialize · D4b input_viewport · D4c bake poll · D4d menus |
| **Risk** | **Low** |

---

## 8. Phase D5 — Dead enum variants (serialize-guarded)

### Hard rules for D5

1. **Serialize check first** (see §1). No variant removal without it.
2. Owner-checked safe enums (no `Serialize`/`Deserialize`): `PastePosition`, `WorkClass`, `PipelineReady` — **tags won’t corrupt `.fl4`**, but product/architecture rules still apply.
3. **`WorkClass::Critical` / `High` / `IdleOnly`** — **KEEP + document**. These are FrameScheduler architecture variants, not junk. Do **not** delete. If the compiler warns, use `#[allow(dead_code)]` with a comment pointing at the scheduler design, **or** wire them later — never strip.

| Variant | Action |
|---------|--------|
| `PastePosition::Offset`, `WorldPoint` | After serialize check (already non-Serialize): delete **or** `#[allow(dead_code)]` if paste API is WIP. Prefer allow if product still wants paste modes. |
| `WorkClass::Critical/High/IdleOnly` | **KEEP + document.** No delete. |
| `PipelineReady::DeferredLite` | Grep **send** sites. If only a receive arm remains → remove variant **and** match arm together (non-Serialize confirmed). |

| | |
|--|--|
| **Sub-slices** | **D5a** PastePosition · **D5b** PipelineReady::DeferredLite · **D5c** WorkClass = **annotate/document only** (no strip) |
| **Risk** | Low for Paste/PipelineReady after grep; **zero-delete** for WorkClass |

---

## 9. Phase D6 — Product-surface APIs (owner gate · hold)

Clusters that look dead but may be “UI not wired yet.” **Do not agent-strip without explicit keep/kill.**

| Cluster | Symbols (examples) | Owner question |
|---------|-------------------|----------------|
| **Clipboard prims** | `copy/cut/paste_selected_prims`, `prim_clipboard`, `PasteResult::is_empty` | Shipping prim clipboard? |
| **Array dup** | `start_shift_dup_array`, toolbar, confirm, … | Tool still planned? |
| **Warming** | `WarmingState` fields + `record_activation`, `pick_warming_candidate`, `poll_warming` | Warming feature on? |

| | |
|--|--|
| **Process** | Per cluster: **KEEP** (`#[allow(dead_code)] // <reason>`) or **STRIP** (one PR per cluster) |
| **Verify strip** | cargo check + **owner smoke** (not agent-only) |
| **Sub-slices** | D6a clipboard · D6b array-dup · D6c warming |
| **Risk** | Med–High if planned product surface is deleted |

---

## 10. Phase D7 — Bake / brick / classify / LOD ⚠️ RECAST: KEEP + annotate

### Why this is not a strip phase

Owner-verified **live** usage (false “dead” under naive inventory):

| Cluster | Live evidence (do not delete) |
|---------|-------------------------------|
| **`bake_body_grid*`** | Live bake path (`app_poll_bake.rs`), save/load re-bake (`project.rs`), DC oracle, 15+ sparse-bake tests |
| **`BrickGridAccessor` / `classify_*`** | Active Phase-3 sparse-SDF work across ~26 files (`sdf_brick_atlas.rs`, `brick_classify.rs`, spikes, etc.) |

Deleting D7 “strip” clusters would break **sparse-SDF pipeline** and **save path**.

### Correct D7 action

| | |
|--|--|
| **Action** | **KEEP** all bake/brick/classify/LOD clusters listed below |
| **If compiler warns** | `#[allow(dead_code)] // Phase-3 oracle / sparse-SDF / live bake — not dead` (or more specific reason) |
| **Treat “dead” warnings here as** | **False positives** from test-only / cfg-gated / cross-crate callers until proven otherwise with full `cargo test` + grep |
| **Delete** | **Nothing** in D7 unless a *future* owner re-audit proves a *single* symbol has zero callers including tests, spikes, and plan-doc oracles |

| Cluster | Examples | Disposition |
|---------|----------|-------------|
| Classify | `finalize_gpu_classify_verdicts`, `classify_bricks_coarse`, `submit_classified_request` | KEEP + allow if warned |
| Brick fill | `ensure_brick_fill_pipeline`, `PendingBrickFill` fields, `brick_fill_pending` | KEEP + allow if warned |
| Body vol | `set_brick_pool_enabled`, `has_cached_grid_at_res`, `entry_bounds`, … | KEEP + allow if warned |
| LOD | `lod_full_window`, `age` field | KEEP + allow if warned |
| Bake grids | `bake_body_grid`, `bake_body_grid_region` | **KEEP — live path** |

| **Risk if stripped** | **Catastrophic** (save + sparse bake) |
| **Risk if annotated** | **None** |

**Sub-slices (annotate-only):** D7a classify · D7b brick fill · D7c body vol API · D7d bake_body_grid · D7e LOD  

---

## 11. Phase D8 — GpuCtx / pipelines (owner smoke · BGL-first)

| Item | Notes |
|------|--------|
| `raymarch_pipeline_layout` | Confirm nothing binds it |
| `brick_pool_dummy_buf`, `brick_info_buffer`, `lab_mesh_grid_buffer` | **BGL rule** — may be load-bearing even if never read in Rust |
| `temporal_dummy_mask` | Confirm temporal path |
| `DeferredLite` pipeline ready path | Pair with D5b only after serialize/send grep |

| | |
|--|--|
| **Pre-delete** | Grep `*_bgl_entries()` + bind-group builders for every candidate |
| **Agent can do** | Grep report + cargo check of a *proposed* patch in a branch |
| **Agent cannot finish** | “Launch app; Solid + PT + resize; no missing-binding panic” — **owner** |
| **Risk** | **Med** |
| **Hold** | Until owner is free for smoke |

---

## 12. Phase D9 — UI / outliner / input (owner smoke)

| Item | Notes |
|------|--------|
| `OutlinerDeferred` unread fields | Deferred action bus — **KEEP or wire**, don’t half-delete |
| `ActionEntry::{name,category}`, `actions_in_category` | Keybinding UI incomplete? |
| Sculpt `exit_sculpt_mode`, `discard_sculpt_restore_sdf` | Confirm menu paths |
| `draw_props_body_mirror`, `enter_pivot_mode` | UI remnants |

| | |
|--|--|
| **Risk** | **Med** |
| **Verify** | Owner interactive smoke |
| **Sub-slices** | D9a outliner · D9b input system · D9c sculpt exit · D9d props |
| **Hold** | Until owner-in-loop |

---

## 13. Phase D10 — Final audit (½ session + owner smoke)

| | |
|--|--|
| **Agent** | Re-run warning harvest; compare to D0; list remaining `#[allow]` with reasons |
| **Owner** | Manual smoke: Solid, PT orbit, pick, resize — no missing-binding panic |
| **Done when** | Warning count down; remaining allows documented (short note / ticket / this file’s log) |
| **Verify** | Full `cargo check -p app -p graphics -p render -p core_math` + `validate_pt_tri_shader` + **owner smoke** |

---

## 14. Session playbook (copy-paste)

```
1. Pick one ID (e.g. D1)
2. Grep every symbol (and BGL/serialize rules if applicable)
3. Delete only proven-dead items OR annotate with #[allow] + reason
4. cargo check / naga as required
5. Note remaining warnings for that area
6. STOP — do not start the next slice in the same session unless trivial
```

**Agent-complete IDs:** D1, D2, D3, D4 (and D5a/b only after serialize grep; D5c annotate-only; D7 annotate-only).  
**Owner-required IDs:** D6 keep/kill, D8 smoke, D9 smoke, D10 smoke half.

---

## 15. Risk register (corrected)

| Risk | Mitigation |
|------|------------|
| Delete “unused” API that is live under tests/spikes | **D7 = keep+annotate**; full-repo grep + `cargo test` before any future strip re-open |
| Delete serialized enum variant → corrupt `.fl4` | **Serialize check** on every D5 delete |
| Delete FrameScheduler architecture variants | **WorkClass Critical/High/IdleOnly = keep+document** |
| GPU dummy still in BGL | Grep `*_bgl_entries()` before D8 |
| WGSL still referenced by string assembly | Grep whole `graphics` + `shader_build` (D1) |
| Agent claims smoke done headless | **Forbidden** — owner launches app for D8/D9/D10 |
| Mega-PR conflict | One slice = one PR / one revert |

---

## 16. Immediate next actions

1. **Execute D1** — five PT WGSL orphans only; naga validate.  
2. **Execute D2 → D3 → D4** in separate sessions (or sequential commits if context allows; still one ID per commit).  
3. **Do not open D7 as strip.** If warnings fire on bake/brick/classify, annotate only.  
4. **D5:** serialize check → D5a/b optional; D5c WorkClass annotate/document only.  
5. **Hold D6 / D8 / D9** until owner free for keep/kill + smoke.

When executing: say **GO D1** / **EXECUTE D1** for the first slice if not already authorized by “ship D1–D4 now.”

---

## 17. Definition of “strip complete”

- [ ] D1–D4 merged  
- [ ] D5: safe variants done **or** allowed; WorkClass kept + documented  
- [ ] D6: each cluster either stripped (owner OK) or `KEEP + allow + reason`  
- [ ] D7: **no strips of live clusters**; allows documented where needed  
- [ ] D8–D9: owner-smoked decisions  
- [ ] D10: baseline comparison documented  
- [ ] No behavior change; owner smoke OK  

**B1 G-buffer is not part of this plan.**

---

## 18. Changelog from original plan

| Change | Reason (owner) |
|--------|----------------|
| **D7 recast strip → KEEP + `#[allow]`** | `bake_body_grid*` and brick/classify are live (bake, save, Phase-3 sparse-SDF, tests) |
| **D5 + serialize/deserialize guard** | Bincode tag shift corrupts `.fl4`; named enums happen to be safe but rule is mandatory |
| **D5c WorkClass → keep+document** | FrameScheduler architecture, not dead junk |
| **D8/D9/D10 smoke = owner** | Launch app cannot be agent-finished (exe lock / headless) |
| **D8 + BGL grep rule** | Dummy GPU resources can be layout-load-bearing |
| **D1–D4 confirmed ship-now** | Only fully agent-finishable, cargo-check verifiable slices |
| **D6/D8/D9 hold for owner-in-loop** | Product surface + interactive verify |
