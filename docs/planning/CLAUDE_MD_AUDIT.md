# CLAUDE.md — Rules Audit

**Date:** 2026-07-15 · **HEAD:** e6d3f9e · **Method:** read-only, no builds, no source edits
**Scope:** all 402 lines of `CLAUDE.md`, ~105 discrete rules
**Calibration example:** the Input System Discipline rule (rewritten today, commit e6d3f9e) — a rule whose
literal compliance produced the inverse of its own intent. That is what "rotted" looks like. It is NOT
re-reported here.

---

## 0. THE HONEST VERDICT

**CLAUDE.md is structurally sound and citation-rotten.** It is neither "mostly healthy with a few rotten
spots" nor "systematically stale." The precise finding:

> **The judgment layer survived. The reference layer rotted.**

Not one rule in this file is wrong about *what to do*. Twenty-five are wrong about *what things are called*.

### Tally (105 rules)

| Verdict | Count | % | Meaning |
|---|---|---|---|
| **A. CURRENT** | 54 | 51% | Accurate + load-bearing. Leave alone. |
| **B. STALE-FACT** | 25 | 24% | Rule right, citation rotted. Fix the pointer. |
| **C. TOMBSTONE** | 7 | 7% | Guards deleted code. **KEEP — all 7 held under adversarial review.** |
| **D. WRONG** | 8 | 8% | Misinstructs today. Rewrite. |
| **E. OBSOLETE** | **0** | **0%** | **Nothing is safe to delete.** |
| **F. DUPLICATE** | 1 | 1% | (+5 noted as secondary overlaps) |
| **G. ASK-OWNER** | 10 | 10% | Owner intent required. |

**The number that matters most is the zero.** Five section auditors plus two adversarial challengers
examined ~105 rules and produced **zero** class-E deletions. Every "delete" candidate that surfaced was
`compute.wgsl`-shaped — a file that no longer exists but whose *intent* may still be worth preserving —
and every auditor who touched it correctly declined to decide, and escalated. That restraint is the
correct outcome, not a shortfall. A wrongly-deleted guard is unrecoverable.

**Every one of the 25 STALE-FACTs is a name, number, or pointer.** Zero are principles. That is the
diagnosis: this file's rules are good and its citations are unmaintained, because *nothing checks them*.

---

## 1. CHALLENGE ADJUDICATIONS

Two challengers reviewed the five section audits. I verified the contested claims myself rather than
averaging. Decisions:

### 1.1 Challenges that LANDED — applied

| # | Challenge | Decision | My verification |
|---|---|---|---|
| **C2-1** | **`SDF Body Rendering — Triangles Only` (L69–74) was assigned to NO auditor.** Section 1's brief said "L7–97" but its rule list skips it; Section 2 starts at L99. | **APPLIED.** Verdicts rendered below (§2.0). | Confirmed by reading both briefs. The block contains a top-tier tombstone (L73) and half of a new contradiction (L71). |
| **C2-2** | **L328 is a FALSE `CURRENT`.** Auditor 4 verified `VolumeAtlas.texture_generation` *exists* and inferred it is *live*. It is not. | **APPLIED — reclassified A → B.** The single most important catch in the review. | **Verified myself:** `VolumeAtlas::new`/`new_auto` have **zero** callers repo-wide (only two comments citing them as *precedent*, `gpu_ctx.rs:523,601`). `app_update.rs:857` = `volume_atlas: None` under the comment *"STRIPPED 2026-07-13 … **Never created** — all consumers are Option-tolerant."* `viewport_render.rs:865`: *"gpu.volume_atlas is no longer created."* `texture_generation += 1` (`volume_atlas.rs:657`) is unreachable code. |
| **C2-3** | **NEW contradiction: L71 vs L328.** L71 forbids the 812³ `VolumeAtlas`; L328 holds `VolumeAtlas.texture_generation` up as the model to copy. | **APPLIED.** See §3.1. | Verified. And the pattern *is* spreading correctly — live code cites the dead anchor by name: `material_textures.rs:25`, `sdf_brick_atlas.rs:66`. Pattern healthy, anchor a corpse. |
| **C1-2** | **Auditor 1's `volume_preview` note is FALSE.** They told the owner `volume_preview.wgsl` is "still compiled for the sculpt raymarch — legitimate." | **APPLIED — Auditor 1 overruled, Auditor 3 upheld.** | **Verified myself:** `volume_preview_sculpt_pipeline` is a **name only**. `app_poll_pipeline.rs:31` assigns `PipelineReady::SculptRaymarch(p)` to it; `app_update.rs:595` builds it from `build_sculpt_voxel_raymarch_wgsl()` with `entry_point: "sculpt_voxel_raymarch"`; it binds `sculpt_raymarch_bgl_entries()` (`bgl.rs:237`). `vol_preview_bgl_entries` (`bgl.rs:192`) = **1 hit, its own definition, zero callers.** `build_volume_preview_wgsl` = zero production callers (tests only, `shader_build.rs:507,532`). Auditor 1 was fooled by a field name — ironically the exact "naming collisions manufacture false staleness" theme Auditor 5 named. Left uncorrected, their note would have preserved dead scaffolding on a false rationale *and* left the L71 tombstone contradicted. |
| **C1-1** | **FL4MATTX: reject Auditor 4's `WRONG` + permissive rewrite.** | **APPLIED — reclassified D → B.** See §4.1 for the full reasoning; this is the closest call in the audit. | Verified `MATERIAL_MAP_MAGIC = b"FL4MATTX"` (`project.rs:387`) and the loader refactor (`project.rs:385–388`). |
| **C1-6** | **Transaction Model: reject the "4th exception," reframe so `poly_op_batch` CONFORMS.** | **APPLIED — strictly better than Auditor 2's proposal.** | Naming an exception to "No exceptions" invites a fifth. `poly_op_batch` (`undo.rs:384`) already routes to the same undo stack via `UndoPayload::Poly`; its own doc says *"no parallel undo path, CLAUDE.md."* It violates only the literal function *name*, not the invariant. Reframe to "exactly ONE undo entry via a sanctioned path" → it conforms, and "No exceptions" stays literally true. |
| **C1-8** | **Display Invariant L29: minimal fix only; mechanics belong in L47.** | **APPLIED.** | L29 sits inside a crash-class invariant whose job is one sentence. Injecting storage-class mechanics dilutes a device-loss guard into trivia that re-rots. Number fix at L29; mechanics at L47. |
| **C1-4** | **App Identity: keep the prescription, don't demote it to a description.** | **APPLIED.** | Auditor 5's "Data roots (3, historical)" is a rule that has surrendered — the next dev adds a 4th root and is compliant. Keep `%LOCALAPPDATA%/Azra/` as the canonical target, list the three actual roots as a **warning**, escalate consolidation. Their refusal to migrate the code is correct and preserved. |
| **C1-7** | **Materials: reject retitle option (b) "Referenced Or Embedded."** | **APPLIED.** | The code votes for (a): `primitives.rs:171` calls embedded a *"(migration default)"*; `properties_sdf_material.rs:80` labels the choice *"Clear material ref"* — an **absence** of a reference, not a first-class mode. Option (b) legitimizes the path and kills the direction of travel. Present it to the owner as the option that abandons the architecture, not a neutral alternative. |
| **C1-9 / C2-4** | **Reject "fewer file:line citations"; adopt the CI grep instead.** | **APPLIED.** | Citations are the only reason this audit produced evidence instead of opinion. Stripping them makes rules rot *silently* instead of *visibly*. See §5. **C2's caveat is also correct and preserved:** the grep would NOT have caught L328 — "the symbol exists" is exactly what such a gate proves and exactly what was wrong there. Liveness needs eyes. |
| **C2-5** | **Auditor 1's L46 verdict is incomplete** — caught the dead `:1793` line, missed `viewport_render()` in the same sentence. | **APPLIED.** | **Verified:** `grep "fn viewport_render"` = **zero**. Renamed to `dispatch_gpu_render` (`viewport_render.rs:17`); only the *module* kept the old name, which is why the symbol looks alive. **4 live sites: L46, L48, L320, L321.** Neither auditor had the whole list — the concrete cost of the section split. |
| **C2-6** | **The mis-addressed rules are a class, not two incidents.** | **APPLIED.** | `grep -n "prompt must" CLAUDE.md` = exactly 2 (L173, L381). Both instruct the prompt-writer in a document the agent reads. Auditor 2 marked L173 CURRENT purely because `FEATURE_RULES.md` exists — never asking whether the sentence can fire. |
| **C2-7** | Three more missed rules inside Section 1's own range: **L28, L48, L54.** | **APPLIED** — all three CURRENT, verdicts in §2.0. | L48's generation counter is verified alive (`voxel_gpu_mesh.rs:49`) — ironic, given L328's cited one is dead. |

### 1.2 Challenges REJECTED — and why

| # | Challenge | Decision | Why |
|---|---|---|---|
| **C1-3** | Defender: **keep the BEVEL.md "~22" count** (bump to ~24); reject Auditor 1's "delete the number." | **DEFENDER UPHELD — bump to ~24.** | Auditor 1's stated reason ("any literal count is guaranteed to rot again") is defeated by the rule's own text: the **`~` already marks it approximate**, so it cannot rot in a load-bearing way — ~22 vs ~24 changes no behavior. The count *is* the deterrent: "~24 failed approaches" is what makes a dev feel the weight before deriving #25; "the graveyard §6" is a mere pointer. This project *has* a 24-entry bevel graveyard precisely because dead ends get re-derived. Cost of deleting: strips the only rhetorical force from the most scar-tissue-heavy rule in the file, to fix a non-problem. |
| **C1-5** | Defender: **Bug Tracker is a coverage gap (B/G), not `WRONG` (D)** — "nobody following it does damage." | **SPLIT DECISION — class rejected, fix accepted.** Two separate claims, two decisions (not an average). | **Class: Auditor 5 upheld — it stays `WRONG`.** A rule that *structurally cannot fire* is not merely under-covered; it fails to instruct the party who reads it. The 29% compliance (2 of the last 7 `fix(` commits touched the tracker) is the proof. **Fix: defender upheld — make it ADDITIVE.** The prompt-writer clause is a genuine instruction to the *owner*, who also reads this file; deleting it loses that. Keep it, ADD the agent-addressed sentence. Same treatment for L173. |
| **C2-8** | Challenger: adjudicating the suspected **Scope Control (L95) vs No Duplication (L206)** conflict. | **NOT A CONFLICT — upheld.** | L206's verb is **flag**, not extract: *"flag it for extraction even if extraction isn't the task."* That word is what makes it compose with "Fix ONE bug per task" and the 1-extraction budget. **Do not let a tidying edit soften "flag" to "extract."** |

---

## 2. RULE-BY-RULE VERDICTS

### 2.0 THE UNASSIGNED BLOCK — `SDF Body Rendering — Triangles Only` (L69–74) + L28, L48, L54

*No auditor owned these. Rendering verdicts now.*

| Rule | Verdict | Evidence | Proposal |
|---|---|---|---|
| **L70** — SDF display is triangles-only in EVERY mode, from the baked DC mesh via the raster pass | **CURRENT** | `RenderMode` (`app_types/src/types_render.rs:9-18`) holds only `PathTrace` + `AnalyticalLite`. No SDF volume/analytical display path exists. | Leave alone. |
| **L71** — DELETED never reintroduce: `RenderMode::VolumePreview`, the VP pipeline, **the 812³ `VolumeAtlas`**, `force_volume_only`, `sdf_edit_active` | **TOMBSTONE** (with one correction) | Variant gone (`types_render.rs:12` tombstone). `force_volume_only` = 13 hits, all comments. **BUT:** the `VolumeAtlas` *struct* survives compiled-but-uninstantiated — `gpu_ctx.rs:435 Option<VolumeAtlas>`,永 `None` (`app_update.rs:857`). | **KEEP.** Add: *"the `VolumeAtlas` struct still compiles but is **never constructed** (`app_update.rs:857`, always `None`; all consumers Option-tolerant). Deleted means never created, not never compiled — do not 'restore' it, and do not cite it as a live pattern (see L328)."* |
| **L72** — BAKE infrastructure (`volume_bake.wgsl`, mode-2 sparse, GPU classify, `ResidentVolume.rv.buffer` field-direct DC) unchanged + load-bearing | **CURRENT** | All named artifacts live; the bake is the production SDF display path. | Leave alone. Load-bearing counterweight to the tombstones around it — it tells the reader what *survived* the deletion. |
| **L73** — **Analytical SDF display is DELETED (2026-07-14) — never reintroduce** | **TOMBSTONE** | `graphics/src/glsl/` does not exist. `glsl_pipeline` = 0. Every hit for `raymarch_entry`/`raymarch_march`/`GlslBvh`/`AnalyticalMarch`/`AnalyticalFull`/`pipeline_march` is a tombstone **comment**. | **KEEP VERBATIM.** One of the two strongest tombstones in the file. **One clause is false:** *"`bvh_enabled` … no longer exist."* **Verified:** `graphics/src/types.rs:79 pub bvh_enabled: u32`, `:363` default 0, hardcoded 0 at `viewport_render.rs:781` *("uniform slot kept, always 0")*, + 5 WGSL struct decls. `types_render.rs:126-127` says the slot stays deliberately. Reword → *"the flag and its code paths are gone; the GPU uniform SLOT stays, hardcoded 0 — do not wire it back, do not delete it (uniform ABI)."* Same fix at **L341**. |
| **L74** — `write_prim_transform` marks dirty regions … full-body bakes debounce at 40ms | **DUPLICATE** of **L56** | Verbatim-near-identical sentences in two sections. | Keep the **L56** copy (DirtyFlags Discipline owns bake/flag semantics); replace L74 with a cross-reference. Two copies = two things to rot out of sync. |
| **L28** — Object Mode (home view) = triangles only, one G-buffer | **CURRENT** | The sculpt raymarch is gated `work_mode == WorkMode::Sculpt` (`viewport_render.rs:237-238`) → Object Mode stays triangles-only. | Leave alone. Worth stating precisely *because* Auditor 1's Plan-B1 ASK-OWNER makes it look threatened. |
| **L48** — Atlas buffer (re)creation + bind-group rebuilds obey Frame Ordering + generation counters | **CURRENT** | `VoxelAtlasGpu` carries `pub(crate) generation: u32` (`voxel_gpu_mesh.rs:49`) + a combined-generation guard for referenced-but-not-owned resources (`:57, :571`). | Leave alone. (Only fix: the `viewport_render()` rename touches this bullet — see §5.1.) |
| **L54** — Never set SCENE for prim-level property edits; `rebake_no_undo()` sets SCENE for the prim buffer upload | **CURRENT** | `rebake_no_undo` (`dirty.rs:128`) ends in `mark_dirty(DirtyFlags::SCENE)` (`dirty.rs:150`). | Leave alone. Non-trivial that it survived, given L51/L53's light claims (same function) are false. |

### 2.1 Priority Ladder + early Non-Negotiables (L7–97)

**CURRENT (leave alone) — verified, next auditor may skip:** Priority Ladder (L8–13, `scene_state.rs:17` + `prim_actions.rs:412/438`) · Rule Conflict Check (L24, unfalsifiable process rule) · Display Invariant core (L27/L30/L31 — **TOMBSTONE**, all 8 forbidden symbols grep to comments only) · Mutation Discipline (L35–39, `prim_actions.rs:412/438`, `properties_sdf.rs:28`) · Voxel Sculpt GPU authority (L44, `app_poll_voxel.rs:66-70`, `input_sculpt.rs:1794-1799`) · DirtyFlags RENDER + dirty_bodies independence (L52/L55/L56/L57 — the `||` at `dirty.rs:108-109` *is* the independence claim; `DEBOUNCE_MS = 40` at `bake_dispatch.rs:19`) · Prim Position model (L60–65, `scene_graph.rs:1689/1703/1708`) · Scope-Leave Bake Dispatch (L77–78, `resident_volume.rs:70-78` near-verbatim) · **Structural Mutation Checklist (L81–88)** · Build L92 · Scope Control (L95–97).

> **Structural Mutation Checklist (L81–88) — the healthiest scar tissue in the file.** All 4 fns exist
> (`scene_graph.rs:1161`, `prim_actions.rs:783`, `prim_actions.rs:275`, `dirty.rs:195`); **both** bug IDs are
> real (CR-71 `COMPLETE_BUG_TRACKER.md:2376`, CR-74 `:2379`); and it is **still catching bugs 3.5 months
> later** — the 2026-07-13 outliner DnD work cites it live (`:2597`, *"old parent was previously NOT dirtied
> — CR-74 class"*). **KEEP VERBATIM. Do not "modernize."**

| Rule | Verdict | Evidence | Proposal |
|---|---|---|---|
| **L29 + L47** — voxel display "raster mesh ≤512³ / atlas raymarch >512³, auto by resolution with manual override" | **WRONG** (both sites, one root) | **Verified myself:** `voxel_sculpt.rs:1472 pub const SPARSE_RES_THRESHOLD: u32 = 256;` → `:1530 if res >= SPARSE_RES_THRESHOLD`. Display gate has **no resolution term**: `viewport_render.rs:237-238` keys on `.density.is_sparse()`. "Manual override" does not exist (6-term grep = 0). Rot origin: **512 is the *SDF* volume-cache cap** (`body_volume_cache/mod.rs:738`) that bled into a *voxel* rule. | **L29 (minimal — per C1-8):** "voxel → raster mesh (dense) / atlas raymarch (sparse, ≥256³)". **L47 (owns the mechanics):** "Display = atlas raymarch when the grid is SPARSE (`res >= SPARSE_RES_THRESHOLD` = 256, `voxel_sculpt.rs:1472`); dense (<256³) → chunk raster mesh. Storage class, not a resolution test. **No manual override.** Under sparse the triangle mesh is demoted to shadow/picking/export only (`viewport_render.rs:233-235`)." |
| **L32** — "`VIEWPORT_MODEL.md` is authoritative" | **WRONG** | Doc exists (repo **root**, not `docs/`) but **contradicts CLAUDE.md and mandates DELETED code**: `:51` *"SDF │ volume raymarch of that body (smooth, live)"*; `:39` *"the smooth volume raymarch is kept for SDF Edit Mode."* CLAUDE.md:29 says baked DC triangles; L73 says that raymarch was deleted. Doc dated 2026-06-19 (pre-deletion) and `:2` still claims precedence: *"CLAUDE.md and MASTER_WORK_LIST will be re-aligned to this doc."* | **See §3.2 — highest-severity contradiction.** Two-part: (1) patch the doc (`:39`, `:51`, strike `:2`'s precedence claim + `:35`'s "volume-only mode"); (2) narrow CLAUDE.md's grant to what the doc is still authoritative *for* — navigation/IA — with CLAUDE.md authoritative for render technique. Fix the path (root, not `docs/`). |
| **L51 + L53** — SCENE sets `light_gpu_data_stale`; "LIGHT also stale on SCENE (light_link_masks depend on prim count)" | **WRONG** (both false) | **Verified myself:** `light_gpu_data_stale = true` has **exactly ONE writer** repo-wide — `dirty.rs:33`, gated on `LIGHT` only. `dirty.rs:29-31` states the **opposite** of CLAUDE.md: *"Light buffer only needs re-serialize when lights actually change. SDF prim edits (SCENE) never modify light data."* Real callers pass flags explicitly: `project_load.rs:437`, `undo.rs:335`. | **L51:** strike `light_gpu_data_stale` → "Sets `prim_buf_stale` (+ `prim_buf_dirty_body = None`), `scene_modified`". **L53:** delete the "Also stale on SCENE" clause — it is false *and* contradicts the next half-sentence of its own bullet. A dev trusting it ships a stale-light bug or "fixes" it in the way the code comment says is wrong. |
| **L51–53** — the flag set is SCENE / RENDER / LIGHT | **STALE-FACT** | **Verified myself:** `app_types/src/lib.rs:13-18` has **FIVE** flags: `NONE`, `SCENE`, `RENDER`, `MESH`, `LIGHT`, `SVO`. **`MESH` is live and undocumented** — `dirty.rs:21` (sets `scene_modified`), `dirty.rs:121-123` (→ `poly_sync_all_meshes()`), 4 setters (`project_load.rs:437`, `project_new.rs:99`, `undo.rs:335`, `undo.rs:507`). **`SVO` is dead** — zero setters; only a debug-HUD read (`viewport_overlays.rs:210`). | **Add `MESH`.** Its omission *causes* the violation this section forbids: a dev doing poly work sees only 3 flags documented and reaches for SCENE — the exact "nuclear option as a convenience shortcut." **`SVO` → ASK-OWNER** (§4, dead flag in bincode-adjacent `app_types`; I will not guess). |
| **L67** — "`BODY_LOCAL_PRIMS` gates all body-local logic … flip to `false` to revert" | **WRONG** | **TWO unlinked consts:** `main.rs:10` (app) and `graphics/src/serialize.rs:11` (graphics, private, `#[allow(dead_code)]`). Not linked (`grep graphics::BODY_LOCAL_PRIMS` = 0). The graphics copy gates **~10 live serializer branches** (`:448, :465, :478, :519, :1552, :1559, :1574, :1757, :2193, :2213`). Flipping one → **double-transformed prims, silently.** | The advertised escape hatch does not work. Preferred: **make it ONE source** (graphics takes it as a param / re-exports the app const) → the rule becomes true as written, and it satisfies the file's own "two code paths MUST share one path" (L196). Else: warn that the revert path is not safe. |
| **L66** — "Save format v6" | **STALE-FACT** | **Verified myself:** `project_load.rs:49 const CURRENT_SAVE_VERSION: u32 = 7;`. Both migration claims still true (`:259` v4→v5, `:343-358` v5→v6). | → **v7** (`project_load.rs:49`). Append the trap the code already knows: *"the version number is NOT the extension signal — a self-delimiting `FL4ORDER`/`FL4MATTX` tail is (`project.rs:378-388`). Do not bump the version for blob-identical additions."* |
| **L20** — BEVEL.md "graveyard of ~22 failed approaches §6" | **STALE-FACT** | Every § pointer resolves exactly (§4@285, §5@309, §6@331, §7@448, §9@541, §11@582). Graveyard is numbered 1..24. | **`~22` → `~24`.** Keep the number (C1-3 upheld — the `~` already absorbs drift; the count is the deterrent). |
| **L43** — "`VoxelObject`/`self.voxel`" | **STALE-FACT** | `self.voxel` no longer exists → `self.sculpt.voxel` (`input_sculpt.rs:456`), moved by `6703d4c refactor(app degod): group 11 voxel-sculpt fields into SculptState`. | `self.voxel` → `self.sculpt.voxel`. Also add `sculpt` to the Subsystem Map (L366), which omits it. |
| **L46** — "so `viewport_render()` re-dispatches … (`input_sculpt.rs:1793`)" | **STALE-FACT** (×2 — C2-5) | `:1793` is now a **blank line**; no "only on release" marker survives (grep = 0). **AND** `fn viewport_render` = **zero hits** → renamed `dispatch_gpu_render` (`viewport_render.rs:17`). | Fix both. **Stop citing line numbers into `input_sculpt.rs`** (3277 lines, actively de-godded — `6703d4c`, `a718110`; this citation has already rotted once). Use a symbol anchor: *"see the `mark_dirty(DirtyFlags::RENDER)` at the end of `apply_one_atlas_restore` (`app_poll_voxel.rs`)."* |
| **L40** — "Active **analytical** unit is always the whole top-level SdfBody subtree" | **ASK-OWNER** | The **invariant is alive** and enforced in 3 places (`dirty.rs:64-67`, `resident_volume.rs:22`, `serialize.rs:239`). But "analytical" names a unit for a renderer deleted 2026-07-14. | Ask: does "analytical unit" mean the **bake/eval** unit? If yes → rename to "Active **bake** unit"; the invariant is real and worth keeping, only the noun is stale. **Do not delete** — the subtree-scoping constraint is enforced in three places. |
| **L47 (2nd clause)** — "Never fused (see Display Invariant)" | **ASK-OWNER** | Code does both and **admits it**: `viewport_render.rs:1153-1158` — *"This is an ADDITIVE overlay pass — the triangle mesher still runs (v1 may double-render; gating the mesh off mid-stroke is a follow-up)."* Genuinely ambiguous in the rule's own text: L30 forbids *multi-object* fusion (not this); L31 **permits** depth compositing *"inside one view only"* (arguably this). | Ask: is Plan B1 an **accepted exception** under L31, or debt the invariant should flag? As written, "Never fused" reads as violated by a shipped, deliberate, commented design — the reader cannot tell intent from debt. (Same failure the new Input rule's EXEMPT clause at L360 was added to fix.) |
| **L91** — "`cargo build -p app` after EVERY change" | **ASK-OWNER** | Discipline right; command questionable twice. (1) **Cannot run as written** — no `Cargo.toml` at repo root; the workspace is `core_engine/`. CLAUDE.md's own L388 correctly says `cd core_engine && …`. (2) `build` vs `check`: owner's recurring feedback says *"exe locked; use cargo check -p app"*, and the project's own latest gate log uses check (`COMPLETE_BUG_TRACKER.md:2597`). | Ask: should this be `cd core_engine && cargo check -p app`? Either way **add the `cd core_engine`** — as written it errors from the documented cwd, which teaches agents to distrust the file. Do not weaken "after EVERY change / fix IMMEDIATELY." |

### 2.2 Stable IDs → Single Responsibility (L99–206)

**CURRENT (verified, skip on re-audit):** Stable IDs (L99–101, `types_transform.rs:32-34` — delta undo *is* keyed by `prim_uid`) · Unified Selection (L104–106, `types_selection.rs:20-21`, 4/4 writers `selection_unified.rs:11/21/38/66`) · No-Duplication practice (L110) · registry entries 4/7 (L112/113/115/117) · Scene Graph Is The Only Owner (L121–124) · Transaction Model mechanics (L128–131, L133) · `MaterialLibrary` serialization-only (L139) · Graph material API (L140, 5/5 fns) · Frame Budget caps (L143 — **all three exact:** `app_poll_bake_poll.rs:56`=1, `app_poll_gpu.rs:768`=2, `:131`=2) · FrameScheduler (L144, 5/5 classes, `frame_scheduler.rs:46 frame_budget_ms: 12.0`) · WarmupManifest (L146) · GpuCtx sub-structs (L150–151, 6/6) · **Outliner Double-Click (L154–155)** · Tool Exit Esc/Enter (L159–161) · Evaluator Over Ad-Hoc (L164–166) · Feature Dev Discipline questions (L174) · No Bandaids (L177–200) · Single Responsibility (L203–206).

> **Outliner Double-Click (L154–155) — the healthiest rule in the file.** Named fn exists
> (`outliner_actions.rs:59`), 4/4 call sites adopted, and the forbidden pattern has **zero** occurrences in
> scope — the only `double_clicked()` string in the outliner is the helper's own doc comment saying
> *"replaces `resp.double_clicked()`."* Guard + adoption + absence of the anti-pattern, all verifiable in one
> grep. **This is the target shape for every rule in this file.**

| Rule | Verdict | Evidence | Proposal |
|---|---|---|---|
| **L114 / L116 / L118** — **the Canonical Function Registry: 3 of 7 entries are fiction** | **STALE-FACT** ×3 — **the worst-rotted item in the file** | **Verified myself:** `fn create_prim` = **0 hits** (the only `create_prim` is an unrelated bool field, `types_tool.rs:157`). `fn delete_prim`/`fn delete_body` = **0 hits**. `recalculate_body_origin` exists but in **`prim_actions.rs:783`**, not `scene_actions.rs`. Real: `add_primitive_immediate()` `creation.rs:65` / `add_primitive_with_op()` `:71`; `delete_selected()` `gizmo.rs:197` → `remove_prims_from_graph()` `prim_actions.rs:135` / `delete_node_no_undo()` `scene_actions.rs:751` / `delete_light()` `:125`. | **Top-priority fix.** The registry is *precisely* what a dev greps before writing code. A dev greps `create_prim`, gets one unrelated bool, concludes "none found" — **and the rule's own reporting ritual then LICENSES the duplicate.** It does not merely fail to prevent duplication; it **manufactures a paper trail justifying it.** Note `recalculate_body_origin` is also step 2 of the Structural Mutation Checklist, whose omission is documented as causing CR-71. |
| **L137** — "`MaterialId::NONE` (0) = default material" | **WRONG** | `resolve_prim_material()` (`graphics/src/serialize.rs:15-24`) returns **`&p.material`** — the prim's **EMBEDDED** material — for NONE *and* for a **DANGLING** id. Embedded field is live (`primitives.rs:170`). UI renders NONE as **"Embedded (local)"** (`properties_sdf_material.rs:67-68`) and offers it as selectable (`:79`). | Rewrite: *"`MaterialId::NONE` (0) = use the prim's EMBEDDED `material: PbrMaterial` (UI: 'Embedded (local)'). Non-NONE resolves from the graph via `resolve_prim_material()` (`serialize.rs:17`); a DANGLING id silently falls back to embedded too."* Actively dangerous as written: a dev either deletes `SdfPrimitive.material` as dead (it is not) or resolves NONE → the Default Material node, **silently restyling every prim on the default path.** The adjacent true L138 makes the false claim *more* credible. |
| **L145** — "`InteractionRenderPolicy`" | **STALE-FACT** | **Verified myself:** `InteractionRenderPolicy` = **0 hits**. Real: `app_state.rs:25 struct InteractionEpisode`, doc'd *"latches pipeline + resolution at episode start"* — the rule's sentence verbatim. 8-frame cooldown confirmed (`viewport_render.rs:140`). **CLAUDE.md contradicts itself:** L185 and L198 use the correct name, as does `FEATURE_RULES.md:23`. | One-word rename → `InteractionEpisode`. The internal inconsistency proves L145 is the outlier. |
| **L149** — "ONE documented owner in `GpuSubsystem`" | **STALE-FACT** | **Verified myself:** `GpuSubsystem` = **0 hits**. Real: `gpu_ctx.rs:395 struct GpuCtx` — **contradicted two lines later by L151** and again by L366. | → `GpuCtx`. Doc reference (`GPU_INVALIDATION_MATRIX.md`) is real; but see §3.3 — its *content* is stale. |
| **L127** — "Every user-visible mutation MUST go through `transaction()` … **No exceptions.**" | **STALE-FACT** | A **third sanctioned path** exists, unnamed: `poly_op_batch` (`undo.rs:384`) + `poly_op` (`:421`), **14 call sites**. Mutates via `Arc::make_mut`, pushes its own `UndoPayload::Poly` (`types_transform.rs:43`). Deliberate + CLAUDE.md-aware: `undo.rs:376-377` *"no parallel undo path, CLAUDE.md."* | **Reframe so it CONFORMS (C1-6 upheld — better than an exemption):** *"Every user-visible mutation MUST produce exactly ONE undo entry via a sanctioned path: `transaction()` (atomic), `begin_episode()`/`commit_episode()` (drags), or `poly_op()`/`poly_op_batch()` (poly Euler ops, O(touched) delta inverses)."* "No exceptions" stays literally true. A dev obeying the current text would wrap poly Euler ops in `transaction()` and **silently discard the delta-undo optimization.** |
| **L158** — "Right-click = cancel. **Exception: Knife**" | **STALE-FACT** | **Loop Cut is a second RMB-confirms exception**, at **TWO sites**: `input_viewport.rs:1718` and a raw-press path `:1328`. The code names the gap: `:124-125` *"Poly Loop Cut: RIGHT-click confirms the cut (owner's choice — a **Knife-style exception to the Tool Exit Convention**)."* | *"Exceptions — RMB CONFIRMS: **Knife** (`:1724`) **and Loop Cut** (`:1718` + raw-press `:1328`)."* Real hazard: a dev enforcing the rule literally routes loop-cut RMB to cancel, killing a deliberate owner-chosen behavior — and would likely "fix" only 1 of the 2 sites. |
| **L132** — "`save_undo()`/`save_delta_undo()` are DELETED" | **TOMBSTONE** | **Zero** definitions, **zero** call sites across `app/src`, `core_math/src`, `app_types/src`. Only 4 orphaned **comments** name them (`gizmo.rs:261`, `stress_test.rs:916`, `undo.rs:231`, `types_transform.rs:65`). Independently gated by `FEATURE_RULES.md:12`. | **KEEP VERBATIM.** A naive grep-count auditor could misread those 4 comments as "still in use" and weaken the rule. Wrinkle worth a low-priority ticket: those comments are false positives for `FEATURE_RULES.md`'s own regression grep (`grep -rn "save_undo" app/src/` → expected 0, actually returns 2), so that checklist item is quietly broken. |
| **L135** — section title "Materials Are Referenced, **Never Embedded**" | **ASK-OWNER** | Embedding is the live fallback for every NONE prim *and* user-selectable. But `primitives.rs:171` calls it a *"(migration default)"* — reads as legacy being phased out. Cannot tell target-state from false-statement. | Ask: is per-prim embedded material **(a)** legacy for v4/v5 migration, slated for removal, or **(b)** a supported authoring mode? **(a)** → retitle "Embedded Is Legacy (Migration Only)" + tombstone the field against new callers. **(b)** → document the NONE→embedded resolution. **Reject option (b)-as-neutral (C1-7):** the code votes (a); (b) legitimizes the path and kills the direction of travel. |
| **L107** — "Entering body edit scope clears unified selection" | **ASK-OWNER** | **Two divergent paths, neither matching the rule.** (A) `outliner_draw_body.rs:167-171`: clears then **immediately re-selects** the entered body → not empty. (B) `input_viewport.rs:1791-1799`: does **not** call `clear_selection_unified()` at all — `remove_from_selection(Node|Prim)`, *"preserve layer/object/light across scope change."* | Ask which is canonical, then restate precisely. **This is also a live violation of L196** ("two code paths for the same operation MUST share one path") — the rule cannot be stated correctly until someone decides which behavior is right. |

### 2.3 Shader Rules (L210–268)

**CURRENT:** 5-file split + `sdf_shared_all()` (L214–219 — 1:1 with `shader_build.rs:51-55`) · `sdf_scene.wgsl` contents + RAY_CULL removal (L217) · deferred/lite builders (L220) · Shader Changes Are Expensive (L224 — "4+" holds at 5–6 builders) · **GLSL/BVH REMOVED (L227 — TOMBSTONE)** · Tetrahedron normals (L230, `sdf_scene.wgsl:384-395`, 4 evals counted) · Feature Must Work in Both Paths (L233–236) · Shader Parity (L245, 41 tests) · Blend Dispatch mechanics (L260–265, 4/4 BlendModes exact) · **`tw!()` safety (L268)**.

> **The 2026-07-14 analytical deletion was done well.** Every tombstone in this section is accurate and each
> has a *matching tombstone comment in the source* (`shader_build.rs:131-133`, `sdf_scene.wgsl:383`,
> `types_render.rs:12-15`). Whoever did that deletion updated CLAUDE.md as they went. **Zero WRONG rules in
> this section.**

| Rule | Verdict | Evidence | Proposal |
|---|---|---|---|
| **L213 / L221 / L292** — `compute.wgsl` "is DEAD code" / "NEVER edit" / "Frozen" | **ASK-OWNER** (3 statements, one nonexistent file) | **The file does not exist.** `git ls-files | grep compute.wgsl` = 0; `find . -name compute.wgsl` = 0 (incl. all worktrees). Deleted in **`5185b2a` (2026-03-29)** *"codebase split — delete compute.wgsl."* Zero pipeline references. | **See §4.2.** It is neither frozen nor dead — it is **gone**; "frozen" + "dead code" was always an incoherent pair and both are now moot. Recommendation (owner's call): collapse 3 → **1 tombstone** under Shader Rules, drop from Frozen Files (leaving only `shader_nodes.rs`). **Not classified OBSOLETE** — the *intent* ("never rebuild a monolithic compute shader") may still be a legitimate guard. |
| **L253** — "Channel packing documented in `graphics/src/lib.rs` — **keep the comment table in sync**" | **STALE-FACT** | Rule right, count right (`lib.rs:77 SLOT_COUNT = 7`, 7 WRITE_* + 7 READ_*). **The table it points at is itself out of sync:** `lib.rs:56` names writers *"raymarch_entry.wgsl AND raymarch_bvh_full.comp"* — **both deleted**. `lib.rs:59` says *"Both writers call `write_gbuf_block(...)`"* — that function's **only occurrence repo-wide is that comment.** `lib.rs:60` says *"MUST be updated in ALL THREE files"* — naming three, two of which are gone. | **Most actively misleading artifact in the section.** Rewrite `lib.rs:52-74` to name the real writers (`raymarch_lite.wgsl`, `sculpt_voxel_raymarch.wgsl`); delete the `write_gbuf_block` sentence + "ALL THREE files". The rule *explicitly says* keep this in sync, and it currently sends devs to two nonexistent files via a nonexistent function. |
| **L256** — "BGL entries for raymarch, deferred, **volume_preview**" | **STALE-FACT** ×2 | **(1) Contradicts L71** (see §3.1). **(2) Omission — the dangerous half:** the rule names 3 fns; `bgl.rs` has **6**: `raymarch_bgl_entries` (`:150`, live), `deferred_bgl_entries` (`:165`, live), `blend_bgl_entries` (`:177`), `vol_preview_bgl_entries` (`:192`, **DEAD, 0 callers**), `bake_bgl_entries` (`:218`), **`sculpt_raymarch_bgl_entries` (`:237`, LIVE — 3 call sites, omitted)**. | Rewrite to what exists and is live: raymarch, deferred, **sculpt_raymarch**, blend, bake. Drop volume_preview. **Highest-risk omission in the section** given the stored lesson *feedback_bgl_binding_sites* (*"6+ BGL sites or bakes fail silently"*) — an incomplete list on a **silent-failure** path. |
| **L264** — "Blend runs … inside the `!skip_deferred_pp` guard" | **STALE-FACT** | Ordering right (deferred `:1255` → fog `:1282` → blend `:1289-1397` → bloom `:1403`). **Guard wrong:** the blend region's enclosing guard is `if render_mode != RenderMode::PathTrace` (`:1227`), which **closes at `:1398`**. `if !skip_deferred_pp {` **opens at `:1399`** — after the blend region — and **contains bloom**. | *"inside the `render_mode != RenderMode::PathTrace` guard (`viewport_render.rs:1227`), immediately before the `!skip_deferred_pp` block."* Semantically inert today (equivalent conditions, `:1114`) but a real trap: **grep the rule's own keyword and you land somewhere that makes you violate the rule's own "BEFORE bloom" clause.** |
| **L248** — verify with ALL build variants "(deferred-full, deferred-lite, **raymarch-lite**, fog, PT, prim-outline)" | **STALE-FACT** (low severity) | Invariant CURRENT and **structurally enforced** (`validate_no_dangling_region_refs`, `shader_build.rs:99-117`, panics at build time). List rotted at both ends: **raymarch-lite has no regions** (`build_raymarch_lite_wgsl` `:137-139` returns raw, never calls `strip_regions`); **omits volume_preview**, which does strip regions (`:260`) and does validate (`:315`). | Fix the list to builders that actually strip regions: deferred-full, deferred-lite, fog, PT, prim-outline (+volume_preview iff kept). Low severity — the validator catches violations regardless — but the list is what a dev reads to decide what to re-check. |
| **L239** — "`apply_boolean_blend()` must NEVER appear in compiled WGSL" | **STALE-FACT** (self-contradicting) | Rule + mechanism verified (`neutralize_blend_noise_wgsl` `:33-39`; regions in **both** files incl. the non-contiguous case `volume_bake.wgsl:818-837` + `:902-914`, exactly as described; gated by `validate_blend_noise_neutralization` `:461-496`). But the sentence is **literally false and contradicts its own next sentence** — the **stub** `fn apply_boolean_blend(` IS injected on purpose, and `:467` asserts it must be present. | Tighten: *"the **NOISY** `apply_boolean_blend` implementation must never appear in compiled WGSL — only the stub."* Source-comment rot to fix alongside: `shader_build.rs:32` still says *"Noisy blends remain functional in the GLSL BVH path"* and `:190` *"GLSL BVH path is unaffected"* — deleted 2026-07-14. **CLAUDE.md is now more current than the code comments.** |

### 2.4 Geometry + File & Code (L272–328)

**CURRENT:** `he.vertex` = DESTINATION (L282, `half_edge.rs:82` comment says exactly this) · `he.prev` / NO_EDGE (L283, 246 occurrences, guard pattern pervasive) · HashMap order (L284) · `normalize_or_zero()` (L285 — **zero** bare `.normalize()` in the governed file) · Poly Kernel no-fork/no-SoA (L295 — `grep bmesh --include=Cargo.toml` = **0**) · PolyMesh wrapper (L296) · **`PolygonalBody` = index 6** (L296 — verified by direct enumeration) · POLY_FOUNDATION_PLAN authoritative (L294) · half_edge freeze lifted (L298–300) · **half_edge serialized-shape red line (L301)** · half_edge bar (L302) · **No Hardcoded Paths (L305 — ZERO violations repo-wide)** · File Organization destinations (L309) · 8MB bincode stack (L314, 5 impls) · **Frame Ordering Contract (L319–323, contract intact)** · Body Mirror AABB (L279) · **Per-Edge Bevel Corner Sphere (L275 — TOMBSTONE)**.

> **L301 (half_edge serialized-shape red line) — actively obeyed under pressure, verified today.**
> `git show ec35abd` (Circle region op, **+790 lines to `half_edge.rs`**, merged `c4f13b1`): the only new
> types are `CircleLoopFit` and `CircleRegionResult`, **both `#[derive(Clone, Debug)]` with no
> `Serialize`** — runtime-only, exactly as directed. **Zero** removals touching any serialized field.
> A rule doing its job on a large same-day change. **KEEP.**

> **L275 (bevel corner sphere) — TOMBSTONE, and the temptation is LIVE.** `grep -iE 'corner_sphere|sphere.*corner'`
> across all `*.wgsl`/`*.rs` = **zero** in any SDF bevel path; revert has held ~4 months. **K83
> (`COMPLETE_BUG_TRACKER.md:1647`) is still OPEN BACKLOG** — so the pressure to re-add it is current. This
> rule is the only thing standing against a proven dead end. **KEEP.**

| Rule | Verdict | Evidence | Proposal |
|---|---|---|---|
| **L315** — "**`NodePayload::VoxelBody` must stay last**" | **STALE-FACT** — **the #1 most urgent item in this audit** | **Verified myself:** `scene_graph.rs` — `VoxelBody {` at **:405**, **`SdfObject {` at :426**. Full order: Root=0 … VoxelBody=**10**, **SdfObject=11**. **The code already knows and says so** (`:400-404`): *"APPEND-ONLY TAIL (VoxelBody = index 10, SdfObject = index 11) … **New variants append AFTER SdfObject only**"*; `:425` *"Appended at index 11 (after VoxelBody)."* | **Fix today.** → *"`NodePayload::SdfObject` is currently last — append AFTER it (`scene_graph.rs:426`; the in-source append-only-tail note at `:400-404` is authoritative)."* **Why urgent despite being "only" a stale fact: the rule now SELF-CONTRADICTS** — you cannot both "append at the END" and "keep VoxelBody last." A dev resolving that contradiction in favour of the stale clause moves VoxelBody back to the tail, shifts SdfObject's index, and **corrupts EVERY existing `.fl4`** — the exact catastrophe the rule exists to prevent, **caused by the rule's own rot.** Classified B not D because the required action ("append at the end") is still right; the remedy is a citation fix. Point the rule **at** the source comment so it cannot rot again. |
| **L296** — "**NEVER a second trailing magic after `FL4ORDER`** (breaks `project.rs:356`'s `deserialize(tail)`)" | **STALE-FACT** (Auditor 4's `WRONG` **overruled** — see §4.1) | **Verified myself:** `MATERIAL_MAP_MAGIC = b"FL4MATTX"` (`project.rs:387`) ships and is gated by a test. The cited hazard was **engineered away**: `project.rs:385-388` — *"Adding this as a SEPARATE magic section is safe ONLY because the loader now reads each section with `deserialize_from` (position-tracked) instead of `deserialize` (which consumed all)."* **Citation also stale:** `project.rs:356` is `pub(crate) struct SavedWpData`, not a deserialize call. | **Fix the citation; KEEP the NEVER.** Append: *"`FL4MATTX` (`project.rs:387`) is the ONE sanctioned exception — it required a loader refactor (`deserialize` → position-tracked `deserialize_from`, `project.rs:436-455`) **plus** a gate test (`:1062-1065`). A third section requires the same bar."* **Reject Auditor 4's 6-line "how to add a tail section" recipe** — it makes section #3 cheap on the one surface whose failure corrupts every save file. |
| **L328** — "**Current implementations:** `VolumeAtlas.texture_generation`, `AtlasSlot.generation`" | **STALE-FACT** (Auditor 4's `CURRENT` **overruled** — C2-2) | **Verified myself:** the fields exist (`volume_atlas.rs:375`, `:145/:177`) and are incremented (`:657`) and read (`:715`) — **but `VolumeAtlas` is never constructed.** `VolumeAtlas::new`/`new_auto` = **zero callers**. `gpu_ctx.rs:435 Option<VolumeAtlas>` is permanently `None` (`app_update.rs:857`: *"**Never created** — all consumers are Option-tolerant"*). `:657` is unreachable. **Meanwhile the pattern is spreading correctly** — live code cites the dead anchor by name: `material_textures.rs:25`, `sdf_brick_atlas.rs:66`. | **The rule is right; its only reference implementation is a corpse another rule bans exhuming (§3.1).** Keep L326–327 verbatim; repoint L328 at live implementations (`sdf_brick_atlas.rs:66`, `material_textures.rs:25`, `voxel_gpu_mesh.rs:49`) and **retitle "Current implementations" → "Reference implementations"** so the list is never expected to be complete (an exhaustive list is itself a rot source). |
| **L276** — "Same radius → `sdRoundBox`. Different radii → smooth interpolation, not hard `max()`" | **CURRENT** (prescriptive, unimplemented) | **The code violates it at 3 live sites:** `sdf_prim_eval.wgsl:445 return max(d_xz, max(d_xy, d_yz));` + `volume_bake.wgsl:294` + `:373`. **`sdRoundBox` appears in NO source file** — docs only. Tracked as **K83** (`COMPLETE_BUG_TRACKER.md:1647`) + BEVEL_QUALITY_PLAN Phase 0. Memory names this the #1 root of the visual-notch bug. | **Keep — but mark it prescriptive.** Phrased descriptively ("→ `sdRoundBox`"), so a dev greps it, finds nothing, and reasonably concludes the rule is stale — the exact rot pattern. Add: *"**NOT YET IMPLEMENTED** — live code still hard-maxes at `sdf_prim_eval.wgsl:445`, `volume_bake.wgsl:294/:373` (**K83**, BEVEL_QUALITY_PLAN Phase 0). Fixing it is a task, not a licence to hard-max in new code."* |
| **L313** — "`map_async` … Never discard with `|_| {}`" | **STALE-FACT** | **Production code fully complies.** But it reads as violated ~10×: all discards are in **test harnesses** (`sdf_gpu_mesh/tests_bake.rs:106/243`, `tests_dc.rs:167/168/218/219/220/275/276/277`). Filtering tests → **zero** non-test violations of 78 `map_async` sites. | **Add a test exemption — the same fix already ratified today for the Input rule (L360).** *"EXEMPT: GPU test harnesses (`sdf_gpu_mesh/tests_*.rs`), which block on `device.poll(Wait)` and would fail loudly anyway — 10 sites, deliberate, do not 'fix'."* Without it the next auditor finds 10 apparent violations and cannot tell intent from debt. |
| **L320 / L321 / L322** — Frame Ordering: "`viewport_render()`" ×2 + "`cache.atlas_rebuild_xy`" | **STALE-FACT** ×2 | **Contract is INTACT and verified:** `app_update.rs:124 poll_background_tasks` → `:128-201` egui panels → `:283` GPU dispatch; no panel file calls `create_texture` (grep = 0). **But:** `fn viewport_render` = **zero hits** → `dispatch_gpu_render` (`viewport_render.rs:17`); the *module* kept the old name. **`cache.atlas_rebuild_xy` does not exist** (grep = 0) — the deferred-flag *pattern* is alive under other names (`cache.material_array_reset_pending`, `app_poll.rs:33-34`; `pending_atlas_restore`; `pending_gpu_classify`). | **Fix both; keep the contract verbatim.** (1) → `dispatch_gpu_render()`, spelled *"(in `viewport_render.rs`)"* to kill the module/function ambiguity. **Repo-wide: L46, L48, L320, L321** (C2-5 — neither auditor had the whole list). (2) Replace the dead example with `cache.material_array_reset_pending`. **A rule whose only concrete example is a symbol that no longer exists trains devs to distrust it — and this contract is too important to be distrusted.** |
| **L308** — "Never add `pub(crate) fn` to `main.rs`" | **CURRENT — but violated once** | **Verified myself:** `main.rs:208 pub(crate) fn bakeprof_record(is_fast: bool, slow_copy_ms: f64)` — a debug bake profiler (env `AZRA_BAKE_PROF`), called from `app_poll_bake_poll.rs:221` + `:315`. Only violation; the other fns are `extern "system"` FFI decls, `fn main()`, `fn drop()`. | **Keep the rule; fix the code** (move to `bake_utils.rs`, where both callers live). **Do NOT weaken the rule to accommodate a debug helper — that is how `main.rs` re-grows.** Flagged only, per Scope Control. |
| **L310** — ">500 lines → ask if multiple responsibilities" | **ASK-OWNER** | **Verified myself:** `main.rs` = **562 lines** — over the threshold — but **149 are `mod` declarations**; 7 fns total. It is a **manifest**, not a god-file; the honest answer to "multiple responsibilities?" is *no*. Memory records an owner target of **~668** for main.rs — a different number than this rule's 500. | Ask: is a raw 500-line threshold still the trigger, now that the de-god work is done and the survivors are manifests? Option: *">500 lines of **logic** (excluding `mod`/`use` manifests)"* + exempt `main.rs`. Not a self-contradiction (it says *ask*, not *split*), but the threshold has no notion of a manifest. |
| **L292** — Frozen Files: `shader_nodes.rs` | **ASK-OWNER** | File is **LIVE** and load-bearing (87KB; `dirty.rs:228-232` → `NodeGraph::build_uber_material_fn`; `properties_sdf_material.rs:108/141`; `app_poll_ai.rs:46`; `tests.rs:148-157`). Freeze is **effective** — untouched since 2026-03-26 (~4 months). **Nothing in the repo says WHY.** | Ask: is the freeze still wanted, and what is the reason? I can prove the file is live and the freeze is obeyed; I cannot find the rationale. **A freeze with no stated rationale is exactly the rule that rots silently** — nobody can tell a deliberate freeze from forgotten scar tissue. Whatever the answer, make the rule carry its reason. |
| **L316** — "recreating ANY buffer → invalidate ALL bind groups" | **CURRENT** (medium — principle verified, not every site audited) | Consistent with the verified generation counters. Partial **DUPLICATE** of L326–328 (this states the *obligation*, L326 the *pattern*) — benign. | Leave. Optional: *"— see GPU Resource Lifetime below for the generation-counter pattern."* |
| **L279** — Body Mirror AABB: "**scope** AABBs" | **CURRENT** (minor term nit) | Implemented as a single source `expand_aabb_for_mirror()` (`bake_utils.rs:227`), 2 callers (`:303`, `bake_dispatch.rs:950`). **Cited by name in code:** `chunked_dc.rs:923` *"(Body Mirror AABB rule — `expand_aabb_for_mirror`)"*. | Leave the rule. `grep scope_aabb|scope_bounds` = **0** — "scope AABB" is not a live term; the live terms are *bake bounds* / *dirty region*. Optionally name the canonical helper: *"expand via `expand_aabb_for_mirror()` — never inline the reflect math."* |
| **L285** — "`normalize_or_zero()` always" | **CURRENT** (one clarifying word) | Zero bare `.normalize()` in `half_edge.rs`. **But `grep 'fn normalize_or_zero'` = 0** — it is a **glam Vec3 method**, not an AZRA function. An auditor can briefly conclude the rule names a nonexistent fn (this happened). | *"**glam's** `normalize_or_zero()` always, never bare `normalize()` (NaN on zero-length)."* |

### 2.5 Good Practices + Build & Run + Workspace + App Identity (L332–402)

**CURRENT:** Architecture-First (L334) · **Analytical Path Status (L341 — TOMBSTONE)** · Feature Dependency Audit (L344–347 — all 3 side-effect owners live: `picking.rs:16`, `pipeline.rs:11`, `shadows.rs:45`) · Subsystem Map (L366 — all 10 fields + 6 GpuCtx sub-structs verified) · Definition of Done (L373–378).

| Rule | Verdict | Evidence | Proposal |
|---|---|---|---|
| **L381** — "Every bug fix **prompt** must include instruction to update `COMPLETE_BUG_TRACKER.md`" | **WRONG** (class: Auditor 5 upheld · fix: defender upheld — see C1-5) | Tracker is **healthy** (399 KB, written today, `da2b46b`). But the rule instructs the **prompt-writer**; CLAUDE.md is read by the **agent**. Measured: **2 of the last 7 `fix(` commits touched it (29%)** — `da2b46b` ✔, `1b63894` ✔ vs `965059f`, `8a1c0a1`, `c843040`, `3ada9ec`, `e9fa024` ✘. | **ADDITIVE fix** (do not replace — the prompt-writer clause is a genuine instruction to the owner, who also reads this file). Keep it; **ADD**: *"When you fix a bug, update `COMPLETE_BUG_TRACKER.md` before the task is done — mark ✅ FIXED + date, or add it if untracked."* Same defect class as the Input rule fixed today: right intent, a shape that cannot fire. |
| **L173** — "Every feature **prompt** must include the standard rules block" | **WRONG** (Auditor 2's `CURRENT` **overruled** — C2-6) | Identical mis-addressed shape. Auditor 2 marked it CURRENT purely because `FEATURE_RULES.md` exists and has 4 questions — never asking whether the *sentence can fire*. It cannot. (L174 *is* agent-addressed and is fine.) | Same additive fix. `grep -n "prompt must" CLAUDE.md` = exactly these 2 hits — **it is a class, fix both together.** |
| **L402** — App Identity: "Data: `%LOCALAPPDATA%/Azra/`" | **WRONG** | **Three roots in production.** (1) `%LOCALAPPDATA%/Azra/` via `dirs::data_local_dir` — `gpu_init.rs:9-14`, `project.rs:515-517`, `:542-544`, `vis_recorder.rs:7`. (2) **`~/.azra/`** via `dirs::home_dir` — **`keybindings.rs:498-500`** (keybinds.json — *the rebind registry the Input rule now points at*), `experimental.rs:21/31`, `project.rs:486` (autosave), `:493` (recent_files), `content_library.rs:122`, `outliner_draw.rs:56`. (3) **`%APPDATA%/Azra/`** (Roaming) via `dirs::config_dir` — `panel.rs:95-97/106-107` (ui_layout.json). On Windows `config_dir()` = Roaming ≠ `data_local_dir()` = Local → genuinely distinct. **None of this violates L305**, which blesses both APIs. | **Keep the PRESCRIPTION (C1-4 — Auditor 5's "Data roots (3, historical)" overruled: a rule that has surrendered blesses the sprawl, and the next dev adds a 4th root compliantly).** *"Data root (canonical target): `%LOCALAPPDATA%/Azra/`. **WARNING — historical sprawl, do NOT migrate existing files:** `~/.azra/` holds keybinds/recents/autosave/experimental; `%APPDATA%/Azra/` holds ui_layout.json. New state → ask before adding a 4th root."* **Do NOT "fix" by consolidating code** — silently orphans every user's settings (the workspaces.json data-loss class). Owner decision → §4. Also: `Struct: AzraApp` → **`FourthLayerApp`, aliased `AzraApp` (`main.rs:533`)** — `grep "struct AzraApp"` = **0**. |
| **L393–399** — Cargo Workspace lists **6** members | **STALE-FACT** | **Verified myself:** `core_engine/Cargo.toml:3-14` has **TEN**: app, **app_ai**, **app_gpu**, **app_types**, core_math, io_bridge, graphics, render, **ui**, cuda_render. `cuda_render` is correctly described (genuinely optional: `default = []`, `cuda = ["dep:cudarc"]`). | **Not cosmetic: `app_types` is where `keybindings.rs` lives** — the crate the Input rule *rewritten today* now points at (`app_types/src/keybindings.rs`, L351). **The map a dev uses to find that file does not list the crate containing it.** Add: **app_types** — shared types, keybindings/ACTION_DEFS, render config; **app_gpu** — GPU plumbing split from app; **app_ai** — AI integration; **ui** — UI crate. |
| **L388** — "`cargo run -p app  # dev (opt-level 2)`" | **STALE-FACT** | **Verified myself:** `Cargo.toml:16-17 [profile.dev] opt-level = 0`. The opt-level-2 the comment remembers applies only to **dependencies** — `[profile.dev.package."*"] opt-level = 2` (*"fixes slow wgpu/naga shader compilation"*). **An undocumented third profile does match the promise:** `[profile.fast-release] inherits = "release", opt-level = 2, lto = "thin", codegen-units = 16`. | → `# dev — app at opt-level 0, deps at 2`. Consider documenting `cargo run -p app --profile fast-release  # opt-level 2, thin LTO`. A dev reading "dev (opt-level 2)" cannot understand why their debug build is slow. |
| **L338** — Brick Pool: "**PARKED** … 23 tracked bugs" | **STALE-FACT** | **PARKED verdict TRUE:** `body_volume_cache/mod.rs:601-602 use_brick_pool: false / use_brick_bake: false`; every consumer early-returns. **Count stale:** BUG-01 (*"remove_body() does not deallocate from brick_pool"*) is **FIXED** — `mod.rs:1321-1322` has exactly the prescribed fix. Audit last touched 2026-03-30, no fix-status tracking. | Keep PARKED verbatim. → *"~23 tracked bugs **as of 2026-03-30** — audit never updated for fixes; BUG-01 is fixed in code. Re-verify before trusting any entry."* **Add the disambiguation — this is the real hazard:** `brick_pool` (PARKED) vs **`brick_allocator`** (`BrickAllocator::new(65536)`, `mod.rs:603` — **LIVE, ungated, under active investment**, commit `9ef06bc` *"S2.6 — sparse brick pipeline is structural at ≥512"*) vs `sdf_gpu_mesh/pool.rs` (the **DC mesh** pool — different subsystem). Three things named "brick"/"pool" with opposite statuses; without the note the next dev reads S2.6 commits and concludes this rule is dead. |
| **L369–371** — Reference Documents: GPU_INVALIDATION_MATRIX.md, VOLUME_ADR.md "(confirmed Week 9)" | **STALE-FACT** | Both files exist. **But GPU_INVALIDATION_MATRIX.md contradicts CLAUDE.md's own tombstone** — see §3.3. **"Week 9" is unverifiable:** the string does not appear in VOLUME_ADR.md (only *":301 developer-weeks"*); the doc self-describes as *"Date: 2026-03-29 / Status: Finalized"*. | (1) §3.3. (2) "(confirmed Week 9)" → **"(Finalized 2026-03-29)"** — the doc's own words; "Week 9" anchors to a calendar nobody can reconstruct. (3) The section lists 2 docs while CLAUDE.md elsewhere cites ~10 — either complete it or retitle so it does not read as exhaustive. |
| **L363** — Vision Doc: priorities from `MASTER_WORK_LIST.md` + `COMPLETE_BUG_TRACKER.md` | **ASK-OWNER** | All 3 docs exist. **The two priority sources have diverged:** COMPLETE_BUG_TRACKER.md = **written today** (`da2b46b`). MASTER_WORK_LIST.md = **2026-06-19, ~4 weeks cold** (self-dated *"Rewritten: 2026-06-18"*). The roadmaps actually being written are elsewhere: `9921d9e` (today) touched `docs/planning/sdf/RESOLUTION_FIDELITY_ROADMAP.md` + `LEGACY_SCULPT_REMOVAL_PLAN.md`. | Ask: is MASTER_WORK_LIST.md still live, or has priority-setting migrated to `docs/planning/`? **Cannot determine from code** — 4 weeks cold is equally consistent with "abandoned" and "stable." **Do NOT delete on staleness alone.** If superseded, the *"AI integration gated on P0–P1"* clause needs a new anchor. Mechanical fix regardless: `Nexux4D.pdf` → **`exported document/Nexux4D.pdf`** (note the space). |
| **L52/L53 (`SVO`)** | **ASK-OWNER** | `DirtyFlags::SVO` (`app_types/src/lib.rs:18`) has **ZERO setters**; only a debug-HUD read (`viewport_overlays.rs:210`). | Dead flag — delete, or a placeholder for planned work? **Not classified OBSOLETE:** it lives in bincode-adjacent `app_types` and I will not guess at intent. |

---

## 3. CROSS-RULE CONTRADICTIONS

### 3.1 🔴 **L71 vs L328** — a tombstone forbids what another rule holds up as the model (**NEW — no section auditor could see this**)

- **L71:** *"DELETED — never reintroduce: … the 812³ `VolumeAtlas`"*
- **L328:** *"**Current implementations:** `VolumeAtlas.texture_generation` (atlas 3D texture)"*

One rule forbids the thing the other says to copy. **And the propagation is real, not hypothetical** — live
code cites the dead anchor **by name**: `material_textures.rs:25` *"GENERATION COUNTER (mirrors
`VolumeAtlas.texture_generation`)"*; `sdf_brick_atlas.rs:66` *"same pattern as
`VolumeAtlas.texture_generation` / `AtlasSlot.generation`"*.

**The pattern is correct and spreading. Only the anchor is a corpse the neighbouring rule bans exhuming.**
Both halves sat in blind spots: L71 was **unassigned**, and L328 was **falsely cleared as CURRENT** by an
existence check that never tested liveness.

**Fix:** keep L326–327 verbatim; repoint L328 at live implementations + retitle "Reference implementations";
add to L71 that the struct survives uninstantiated (deleted = never *created*, not never *compiled*).

### 3.2 🔴 **L32 vs L29/L73** — the authoritative doc mandates deleted code

`VIEWPORT_MODEL.md:51` *"SDF │ volume raymarch of that body (smooth, live)"* and `:39` *"the smooth volume
raymarch is **kept** for SDF Edit Mode"* — while CLAUDE.md:29 says baked DC triangles and L73 says that
raymarch was **DELETED 2026-07-14**. The doc predates the deletion (2026-06-19) and `:2` still claims
precedence over CLAUDE.md.

**Net effect: the tombstones say "never reintroduce the SDF volume raymarch" while the doc they defer to says
"keep it."** A dev obeying the pointer rebuilds the deleted subsystem. **A tombstone undermined by its own
reference.**

### 3.3 🟠 **L341 vs L370** — the tombstone points at a map of the thing it forbids

L341 declares the GLSL BVH / analytical path DELETED. L370 sends the reader to `GPU_INVALIDATION_MATRIX.md`
as the authority on GPU resource ownership — and that doc (frozen 2026-03-30) still lists as **live, owned
resources**: `pipeline (raymarch)` (`:17`), `glsl_bvh_pipeline` (`:20`), `glsl_test_pipeline` (`:21`),
`volume_preview_pipeline` (`:22`), `pt_primary/pt_bounce` as GLSL (`:34-35`), `volume_preview_bg` (`:69`),
`bvh_node_buffer` (`:92`).

**A dev doing the right thing (following the reference) is led straight into the dead end.** Highest
value-per-character fix in the file: **one banner line.**

### 3.4 🟠 **L71 vs L256** — VolumePreview: "deleted" and "maintained" simultaneously

L71 says the VP pipeline is DELETED; L256 names `vol_preview_bgl_entries()` as a BGL you must maintain.
**Reality is a third thing:** the *variant* is deleted, but the *scaffolding survives with zero production
callers* — `vol_preview_bgl_entries` (`bgl.rs:192`, **1 hit = its own definition**), `build_volume_preview_wgsl`
(`shader_build.rs:310`, tests only), and `volume_preview.wgsl`, still compiled and Naga-tested on every
`cargo test -p graphics`. **Auditor 1's claim that it is "still compiled for the sculpt raymarch" is FALSE**
(verified: the sculpt pipeline is merely *named* `volume_preview_sculpt_pipeline`; it is built from
`build_sculpt_voxel_raymarch_wgsl()` and bound with `sculpt_raymarch_bgl_entries()`). → **§4.3.**

### 3.5 🟠 **L315 self-contradicts** — "append at the END" vs "VoxelBody must stay last"

Both cannot be satisfied: `SdfObject` (index 11) is already appended after `VoxelBody` (index 10). **Resolving
the contradiction in favour of the stale clause corrupts every existing `.fl4`.** → §2.4, fix today.

### 3.6 🟡 CLAUDE.md contradicts *itself* on names — the lone mention is always the stale one

| Stale (1 mention) | Correct (2+ mentions) | Verified |
|---|---|---|
| L145 `InteractionRenderPolicy` | L185, L198 + `FEATURE_RULES.md:23` → `InteractionEpisode` | `app_state.rs:25`; stale name = **0 hits** |
| L149 `GpuSubsystem` | **L151 (two lines later!)**, L366 → `GpuCtx` | `gpu_ctx.rs:395`; stale name = **0 hits** |
| L114/L116 registry names | the real creation/deletion fns | `create_prim`/`delete_prim`/`delete_body` = **0 hits** |

### 3.7 🟡 Benign / non-conflicts (do not "fix")

- **L92 vs L302** (`cargo test -p core_math`) — reinforcement, not duplication. L302 is the half_edge red-line
  section. **Do not merge**; the redundancy is load-bearing there.
- **L95–97 vs L206** — **NOT a conflict** (C2-8). L206's verb is **flag**, not extract. **Do not let a tidying
  edit soften "flag" to "extract."**
- **L56 vs L74** — true DUPLICATE (verbatim-near). Keep L56, cross-ref from L74.
- **L143 "Current caps"** — illustrative, not exhaustive (4 more caps exist). The phrasing already says
  "Current caps:", so not a defect.

---

## 4. OWNER DECISION LIST

*The owner said: **"if you are not sure if we need to keep it ask."** These are the cases where code cannot
settle it. Each is intent, taste, or product direction — not something I could have verified and didn't.*

### 4.1 (RESOLVED — recorded for transparency) FL4MATTX: the closest call

Auditor 4 classified L296's *"NEVER a second trailing magic"* as **WRONG** ("the Input-System shape") and
proposed a 6-line permissive recipe. **I overruled it** to STALE-FACT. Reasoning, since it is a judgment call
the owner may want to revisit:

The Input-System analogy fails on the only test that matters. **The Input rule was WRONG because compliance
produced a BROKEN outcome** (non-remappable shortcuts). **Compliance with the FL4 rule produces a SAFE
outcome** — it routes you to the in-blob `#[serde(default)]` path the rule itself names as preferred.
**Over-restrictive ≠ wrong-actioned.**

And Auditor 4's own evidence convicts the proposal: **the rule WORKED.** The FL4MATTX author read the red
line, did the loader refactor (`deserialize(tail)` → position-tracked `deserialize_from`), documented why at
`project.rs:385-388`, and gated it with a regression test whose comment (`:1062-1065`) *names the rule*: *"This
is the exact case the 'no 2nd trailing magic' rule warned about; it is safe now because each section is
length-delimited."* **A red line that makes the second section EXPENSIVE is doing its job.** A how-to makes
the third one CHEAP, on the one surface whose failure corrupts every `.fl4`.

*(Related source-comment rot, out of scope for a CLAUDE.md edit: `project.rs:44` still cites the
`deserialize(tail)` hazard that `:385-388` says was engineered away.)*

### 4.2 ❓ `compute.wgsl` — tombstone or delete? (3 statements, one nonexistent file)

**Facts:** the file **does not exist** (`git ls-files` = 0, `find` = 0 incl. worktrees). Deleted `5185b2a`
(2026-03-29). Zero pipeline references. Stated **3×**: L292 ("Frozen"), L213 ("DEAD code"), L221 ("NEVER
edit"). "Frozen" + "dead code" was always incoherent; both are now moot — **you cannot edit a nonexistent
file.**

**I did not decide this, deliberately.** The *literal* text is inert, but the *intent* may not be: "don't
rebuild a monolithic compute shader" is a legitimate tombstone, and the 5-file-split bullets immediately above
it only make sense against the monolith they replaced. **Ask:** keep as one tombstone (*"`compute.wgsl` was
DELETED `5185b2a` 2026-03-29 — never recreate a monolithic compute shader; SDF code lives in the 5 `sdf_*`
files"*), or drop entirely? Either way: **collapse 3 → 1, and drop it from Frozen Files** (leaving only
`shader_nodes.rs`).

### 4.3 ❓ The `volume_preview` scaffolding — parked by intent, or missed cleanup?

**Zero production callers, still compiled and Naga-tested every `cargo test -p graphics`:**
`vol_preview_bgl_entries` (`bgl.rs:192`), `build_volume_preview_wgsl` (`shader_build.rs:310`),
`build_sdf_eval_block`, `volume_preview.wgsl`, + 2 tests. Meanwhile **L71 declares the VP pipeline DELETED**
and **L256 says maintain its BGL**.

**Ask:** deliberately parked (like the Brick Pool precedent — kept alive, not invested in), or leftovers the
2026-07-13 VP deletion missed? **If parked**, L71 needs a carve-out naming what survives and why. **If
leftovers**, removing it is a **Feature Dependency Audit** (L343), not an audit action. **I am not proposing
deletion.** *(Note: Auditor 1's "it's still used by the sculpt raymarch" rationale is false — do not let it
settle this.)*

### 4.4 ❓ `shader_nodes.rs` freeze — still wanted, and **why**?

File is **LIVE** (87KB, 5 consumers); freeze is **obeyed** (untouched ~4 months). **The rationale exists
nowhere in the repo.** Ask: still wanted, and what is the reason? **A freeze with no stated rationale is
precisely the rule that rots silently** — nobody can distinguish a deliberate freeze from forgotten scar
tissue. Whatever the answer, the rule should carry its reason.

### 4.5 ❓ Data-root canonicalization — which root wins for NEW state?

Three roots ship (`%LOCALAPPDATA%/Azra/`, `~/.azra/`, `%APPDATA%/Azra/`). **Do NOT consolidate naively** —
silently orphans every existing user's keybinds/layout (the workspaces.json data-loss class). Ask: which is
canonical for new persisted state, and is a one-time consolidation-with-migration wanted? Until answered, L402
**actively misroutes**: a dev following it puts a new setting in `%LOCALAPPDATA%/Azra` while the neighbouring
keybinds sit in `~/.azra`. *(Also worth a look: `KeyBindings::config_dir()` is a **keybindings** API that
`content_library.rs:122` borrows as a generic config-dir accessor for a thumbnail **cache** — a cache under a
config root, via someone else's namespace.)*

### 4.6 ❓ Is `MASTER_WORK_LIST.md` still a priority source?

Named in L363 as a priority source; **~4 weeks cold** (2026-06-19) while `COMPLETE_BUG_TRACKER.md` was written
today and today's roadmap commits went to `docs/planning/sdf/`. Cold ≠ dead — equally consistent with
"settled." **Do not delete on staleness alone.** If superseded, the *"AI integration gated on P0–P1"* clause
needs a new anchor.

### 4.7 ❓ Is the Plan B1 sculpt overlay an accepted exception to "Never fused"?

`viewport_render.rs:1153-1158` self-documents: *"an ADDITIVE overlay pass — the triangle mesher still runs (v1
may **double-render**; gating the mesh off mid-stroke is a follow-up)."* L30 forbids *multi-object* fusion (not
this); L31 **permits** depth compositing *"inside one view only"* (arguably this). **Ask:** accepted exception,
or debt the invariant should be flagging? As written, "Never fused" reads as violated by a shipped, deliberate
design — **the next dev cannot tell intent from debt.** If accepted, add an EXEMPT clause (the L360 pattern).

### 4.8 ❓ Does "Active **analytical** unit" (L40) mean the **bake/eval** unit?

The invariant is real and enforced in 3 places; only the noun names a deleted renderer. **Do not delete** —
rename if confirmed.

### 4.9 ❓ Build Discipline: `cargo build` or `cargo check`?

Owner's own recurring feedback says *"exe locked; use cargo check -p app"*, and the project's latest gate log
uses check. **Either way `cd core_engine` must be added** — as written the command errors from the documented
cwd. Do not weaken "after EVERY change / fix IMMEDIATELY."

### 4.10 ❓ `>500 lines → ask` (L310): still the right trigger?

`main.rs` = 562 but 149 are `mod` decls — a manifest, not a god-file. Memory shows the owner tracking main.rs
against **~668**, a different number than this rule's 500. Exempt manifests / count logic lines only?

### 4.11 ❓ `DirtyFlags::SVO` — dead flag, or placeholder?

Zero setters; only a debug-HUD read. **Not classified OBSOLETE** — it lives in bincode-adjacent `app_types`
and I will not guess at intent.

---

## 5. SAFE TO APPLY — mechanical, no owner input

*Ordered by severity. Every one is a name, number, or pointer.*

### 5.1 P0 — save-format landmine (do today)

1. **L315:** `NodePayload::VoxelBody must stay last` → **`SdfObject` is currently last — append AFTER it
   (`scene_graph.rs:426`; the in-source note at `:400-404` is authoritative).** *One word. The rule currently
   self-contradicts in the direction of corrupting every `.fl4`.*

### 5.2 P1 — rules that actively misinstruct

2. **L53:** delete the false clause *"Also stale on SCENE (light_link_masks depend on prim count)"*.
   **L51:** strike `light_gpu_data_stale` from SCENE's effect list. *(Sole writer `dirty.rs:33`, LIGHT-only;
   `dirty.rs:29-31` says the opposite.)*
3. **Registry L114/L116/L118:** `create_prim()` → **`add_primitive_immediate()` / `add_primitive_with_op()`
   (`creation.rs:65/71`)**; `delete_prim()`/`delete_body()` → **`delete_selected()` (`gizmo.rs:197`) →
   `remove_prims_from_graph()` (`prim_actions.rs:135`) / `delete_node_no_undo()` (`scene_actions.rs:751`) /
   `delete_light()` (`:125`)**; `recalculate_body_origin()` → **`prim_actions.rs:783`**.
   *(It currently manufactures a paper trail LICENSING duplication.)*
4. **L137:** `MaterialId::NONE = default material` → **`NONE` = the prim's EMBEDDED `material` field** (UI:
   "Embedded (local)"); dangling ids fall back to embedded too (`serialize.rs:17-24`). Add to L138: *"the
   Default Material node is a convenience entry for assignment; NONE does NOT resolve to it."*
5. **L29 + L47:** `512³` → **sparse/dense at `SPARSE_RES_THRESHOLD` = 256** (`voxel_sculpt.rs:1472`); delete
   *"manual override"* (does not exist). **L29 minimal, mechanics in L47.**
6. **L67:** correct the `BODY_LOCAL_PRIMS` revert claim — **two unlinked consts** (`main.rs:10`,
   `graphics/src/serialize.rs:11`); flipping one double-transforms prims. *(Preferred: unify to one source.)*

### 5.3 P2 — renamed symbols (all verified 0-hit)

7. **`viewport_render()` → `dispatch_gpu_render()` (`viewport_render.rs:17`)** at **all 4 sites: L46, L48,
   L320, L321.** Spell it *"(in `viewport_render.rs`)"* — the module kept the old name.
8. **L145:** `InteractionRenderPolicy` → **`InteractionEpisode`** (`app_state.rs:25`).
9. **L149:** `GpuSubsystem` → **`GpuCtx`** (`gpu_ctx.rs:395`).
10. **L43:** `self.voxel` → **`self.sculpt.voxel`**; add `sculpt` to the Subsystem Map (L366).
11. **L322:** `cache.atlas_rebuild_xy` (does not exist) → **`cache.material_array_reset_pending`**
    (`app_poll.rs:33-34`).
12. **L402:** `Struct: AzraApp` → **`FourthLayerApp`, aliased `AzraApp` (`main.rs:533`)**.
13. **L296:** `project.rs:356` → **`project.rs:378-388`** (`:356` is `struct SavedWpData`).
14. **L46:** drop `input_sculpt.rs:1793` (blank line) → symbol anchor. **Policy: no line numbers into
    `input_sculpt.rs`** (3277 lines, actively de-godded; already rotted once).

### 5.4 P3 — stale numbers, counts, lists

15. **L66:** Save format **v6 → v7** (`project_load.rs:49`). Add: *"the version is NOT the extension signal —
    a self-delimiting `FL4ORDER`/`FL4MATTX` tail is."*
16. **L51–53:** add the missing live flag **`MESH`** (`app_types/src/lib.rs:16`; → `poly_sync_all_meshes()`,
    `dirty.rs:121`). *Its omission causes the SCENE-as-shortcut violation this section forbids.*
17. **L393–399:** workspace **6 → 10 members** (add `app_ai`, `app_gpu`, **`app_types`**, `ui`).
    *`app_types` holds `keybindings.rs`, which the Input rule rewritten today points at.*
18. **L388:** `# dev (opt-level 2)` → **`# dev — app at opt-level 0, deps at 2`**; optionally document
    `--profile fast-release`.
19. **L20:** BEVEL.md `~22` → **`~24`** (keep the number — the `~` absorbs drift; the count is the deterrent).
20. **L338:** *"23 tracked bugs"* → **"~23 as of 2026-03-30, never updated for fixes; BUG-01 is fixed"**. **Add
    the `brick_pool` (PARKED) vs `brick_allocator` (LIVE, `mod.rs:603`) vs `sdf_gpu_mesh/pool.rs` (DC mesh)
    disambiguation.**
21. **L371:** *"(confirmed Week 9)"* → **"(Finalized 2026-03-29)"**.
22. **L363:** `Nexux4D.pdf` → **`exported document/Nexux4D.pdf`**.
23. **L256:** BGL list → **raymarch, deferred, `sculpt_raymarch` (`bgl.rs:237`, LIVE — omitted today), blend,
    bake**; drop volume_preview (pending §4.3). *Highest-risk omission — silent-failure path.*
24. **L248:** variant list → drop **raymarch-lite** (has no regions); add volume_preview iff kept.
25. **L264:** guard → **`render_mode != RenderMode::PathTrace` (`viewport_render.rs:1227`)**, immediately
    before the `!skip_deferred_pp` block.
26. **L73 + L341:** *"`bvh_enabled` … no longer exist"* → **"the flag and its code paths are gone; the GPU
    uniform SLOT stays, hardcoded 0 (`viewport_render.rs:781`) — do not wire it back, do not delete it
    (uniform ABI)."** *(`graphics/src/types.rs:79` + 5 WGSL struct decls.)* **A tombstone disproven by one grep
    gets ignored wholesale.**
27. **L71:** add *"the `VolumeAtlas` struct still compiles but is **never constructed** (`app_update.rs:857`) —
    deleted means never created, not never compiled."*
28. **L328:** **"Current implementations" → "Reference implementations"**, repointed at live ones
    (`sdf_brick_atlas.rs:66`, `material_textures.rs:25`, `voxel_gpu_mesh.rs:49`).
29. **L74:** replace the duplicate with a cross-ref to **L56**.

### 5.5 P4 — precision / exemptions (each prevents a future false-positive audit)

30. **L313:** add the test exemption (10 `|_| {}` in `sdf_gpu_mesh/tests_*.rs`) — **the L360 pattern already
    ratified today.**
31. **L158:** add **Loop Cut** as the 2nd RMB-confirms exception (`:1718` **+ raw-press `:1328`** — two sites).
32. **L127:** reframe to *"exactly ONE undo entry via a sanctioned path: `transaction()`,
    `begin_episode()`/`commit_episode()`, or `poly_op()`/`poly_op_batch()`"* → `poly_op_batch` **conforms**;
    "No exceptions" stays literally true.
33. **L173 + L381:** **ADDITIVE** agent-addressed sentence (keep the prompt-writer clause).
    `grep -n "prompt must"` = exactly these 2 — fix as a class.
34. **L239:** *"the **NOISY** `apply_boolean_blend` implementation … only the stub."*
35. **L276:** mark **"NOT YET IMPLEMENTED — K83"** (3 live hard-max sites).
36. **L285:** *"**glam's** `normalize_or_zero()`"*.
37. **L91:** add **`cd core_engine`** (pending §4.9).
38. **L245:** *"Naga parse tests"* → *"parse **+ validate** tests"* (several now run
    `naga::valid::Validator`, added after a parse-only test let a validation error crash the app —
    `shader_build.rs:351-352`). *Prevents someone downgrading a validate back to a parse.*
39. **L78:** add the 3rd gate the fn's own header names — *"missing, at draft resolution, **or
    regime-unservable**"* (`resident_volume.rs:41-43`).

### 5.6 Doc fixes outside CLAUDE.md (referenced BY rules — the rules' force depends on them)

40. **`GPU_INVALIDATION_MATRIX.md`** — banner: *"STALE as of 2026-07-14: the raymarch / GLSL-BVH /
    volume-preview rows describe DELETED systems."* **§3.3 — highest value-per-character fix in the audit.**
41. **`VIEWPORT_MODEL.md:39/:51`** — SDF Edit → baked DC mesh; strike `:2`'s precedence claim + `:35`'s
    "volume-only mode". **§3.2.**
42. **`VOLUME_ADR.md:141`** — still labels the deleted analytical path **"MAINLINE"**; `:44/:46` still describe
    raymarch bind groups + volume preview.
43. **`graphics/src/lib.rs:52-74`** — the G-buffer table L253 *explicitly says to keep in sync*: names two
    deleted writers + a nonexistent `write_gbuf_block()`. **§2.3.**
44. **`shader_build.rs:32`, `:190`** — still claim the GLSL BVH path is live.

### 5.7 Code violations found (flagged only — Scope Control says fixing them is not this task)

- **`main.rs:208 pub(crate) fn bakeprof_record`** violates L308. Move to `bake_utils.rs` (both callers there).
  **Do not weaken the rule to accommodate a debug helper.**
- **Bevel hard-max ×3** (`sdf_prim_eval.wgsl:445`, `volume_bake.wgsl:294/:373`) violates L276 — **K83**, open.
- **Two body-scope-entry paths** (`outliner_draw_body.rs:167` vs `input_viewport.rs:1791`) violate L196.
- **Two `cam_ray` impls** (`camera.rs:18` vs `transform.rs:313`, the latter conceding *"same math"* at `:310`)
  — deliberate (borrow decoupling) but undocumented in the registry. Name it so nobody adds #3.
- **`save_undo` comment scrub ×4** — they are false positives for `FEATURE_RULES.md:12`'s own regression grep.

---

## 6. THE STRUCTURAL FIX — stop auditing this by hand

**Reject "fewer citations."** Two auditors independently proposed it as the anti-rot lever. Citations are the
**only** reason this audit produced evidence instead of opinion. Stripping them makes rules rot **silently**
instead of **visibly** — and the next audit becomes guesswork.

**Adopt the CI grep instead.** A pre-commit/CI check that extracts every backticked `identifier()` and
`path/file.rs` from CLAUDE.md and **fails when it resolves nowhere in the workspace** would have caught,
mechanically and for free:

> `create_prim` · `delete_prim` · `delete_body` · `GpuSubsystem` · `InteractionRenderPolicy` ·
> `viewport_render()` ×4 · `cache.atlas_rebuild_xy` · `compute.wgsl` ×3 · `sdRoundBox` · `write_gbuf_block` ·
> `self.voxel` · `vol_preview_bgl_entries`(0 callers)

**≈12 of the ~25 STALE-FACT findings**, converted from a recurring manual chore into an enforced invariant —
which is exactly what this file's own **No-Bandaids test #3** ("will the next dev need tribal knowledge? →
enforce correctness structurally") demands of a fix to a recurring problem.

**Two honest limits on that gate:**

1. **It would NOT have caught L328.** "The symbol exists" is precisely what such a gate proves — and precisely
   what was wrong there. **Liveness needs eyes or a dead-code pass.**
2. **Prefer symbol anchors to line numbers inside churning god-files** (`input_sculpt.rs`, 3277 lines, already
   rotted once at L46's `:1793`). Functions survive relocation; line numbers do not. This is compatible with
   keeping citations — it is a precision fix, not a reason to cite less.

**A cheaper structural lever worth naming:** the rules that needed **zero** maintenance across an entire
subsystem deletion — Shader Parity, `tw!()`, Feature Must Work in Both Paths, the Structural Mutation
Checklist — are the ones that state an **invariant** rather than a **location**. And **L315 is the proof case
in reverse**: `scene_graph.rs:400-404` stayed correct while CLAUDE.md rotted, because the comment lives next
to the thing it describes. **Where possible, point AT an authoritative in-source comment rather than restating
a symbol name.**

---

## 7. WHAT SURVIVED — the finding nobody should skip

The brief warned that a naive auditor deletes tombstones. **All 7 held under adversarial pressure:**

| Tombstone | Status | Why it must stay |
|---|---|---|
| **L73/L341** Analytical SDF path | Code genuinely gone; every hit is a comment | The only thing between the next dev and rebuilding a deleted subsystem |
| **L227** GLSL/SPIR-V + BVH | `graphics/src/glsl/` does not exist | Same |
| **L275** Bevel corner sphere | Absent ~4 months; **K83 still OPEN BACKLOG** | **The temptation is live** — proven dead end |
| **L132** `save_undo`/`save_delta_undo` | Zero defs, zero call sites (4 orphan comments only) | 100% obeyed; a grep-count auditor would misread the comments |
| **L301** half_edge serialized shape | **Verified obeyed today under +790 lines** (`ec35abd`) | Corrupts every `.fl4` if broken |
| **L27/L30/L31** Display Invariant | All 8 forbidden symbols → comments only | Device-loss crash class |
| **L71** VolumePreview / VolumeAtlas | Variant gone; struct never constructed | Needs the "survives uninstantiated" clause (§3.1) |

**And three rules are fully-enforced exemplars worth copying:**

- **Outliner Double-Click (L154–155)** — guard exists, 4/4 adopted, anti-pattern has **zero** occurrences.
- **No Hardcoded Paths (L305)** — **zero** violations repo-wide. *Zero violations is evidence the rule works,
  not evidence it is unneeded.*
- **Structural Mutation Checklist (L81–88)** — still catching bugs 3.5 months after CR-71/CR-74
  (`COMPLETE_BUG_TRACKER.md:2597`, a fresh "CR-74 class" miss in the 2026-07-13 outliner DnD work).

---

## 8. METHOD & LIMITS

- **Read-only.** No source edits, no builds, no CLAUDE.md edits. This file is the only artifact written.
- Every verdict is anchored to a `file:line` read or a grep whose result is stated. **Negative claims**
  (`GpuSubsystem`, `InteractionRenderPolicy`, `create_prim`, `delete_prim`/`delete_body`, `save_undo`,
  `fn viewport_render`, runtime `mat_lib`, `VolumeAtlas::new` callers) were each established by exhaustive
  workspace grep — a negative is exactly where a sloppy audit guesses.
- **I could not verify tests are green** (`cargo test -p core_math`, `-p graphics`) — no-build constraint.
- **Unverifiable from code, correctly routed to the owner:** Definition of Done #5 ("manual test scenario
  described") lives in prompt/PR text; MASTER_WORK_LIST intent; the 25–43s recompile figure (near-certainly
  measured when 7 builders consumed `sdf_shared_all()` vs today's 5 — soften to "tens of seconds" or re-time
  once).
- **Zero rules classified OBSOLETE.** Nothing in this file guards a situation I could prove can no longer
  arise. Where I could not settle a question from code, I said ASK-OWNER rather than guess to look decisive.
