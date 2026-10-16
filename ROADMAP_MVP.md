# ROADMAP — to MVP and beyond (the binding map)

**This is the single source of truth for *what we build and in what order.*** It exists so we never
"turn around" — drift back into half-finished areas or chase features before the base is solid.
Companion doc: `STABILIZATION_PLAN.md` (the gate-level execution of Phases 0–1). Status: 2026-06-20.

> Authored from direct code/doc knowledge accumulated this session; maturity marked **(verify)** where
> not yet re-confirmed. A background survey is reconciling the inventory — corrections land as a revision.

---

## 0. How we use this doc (the discipline — agreed 2026-06-20)

1. **Top-down only.** No item is started before its phase is reached. We finish a phase (its exit
   criteria) before opening the next.
2. **Divergence protocol.** If a request — yours or mine — falls outside the current phase, I **STOP
   and say:** *"This diverges from Phase N because X. (a) add to roadmap, (b) defer, (c) consciously
   switch phase?"* Nothing moves silently. You decide.
3. **"Turning-around" detector.** Every item has an **exit criterion**. If we touch the same area a
   second time without its exit met, that is a flag — we name it and decide, not drift.
4. **One change → build → regression checklist → checkpoint commit → next.** (`STABILIZATION_PLAN.md` §6.)
5. **No feature tools below the MVP line** until MVP is signed off.
6. **Effort sizing, not dates.** S = ~1 focused session · M = a few sessions · L = many sessions / its
   own sub-plan. You set the pace; the *order* is what's fixed.
7. **Off-phase bugs → tracker, not now.** When you report a bug that is **not part of the current
   phase's task**, I do NOT drop everything to fix it. I **log it in `COMPLETE_BUG_TRACKER.md`**
   (status `⏸️ DEFERRED`, tagged with the phase/checkpoint it belongs to), confirm to you that it's
   captured so it's never lost, and we fix it **when we reach that checkpoint.** This is the main
   anti-thrash rule — bugs get *recorded*, not *chased*. **One exception:** a genuine **crash or
   data-loss** that blocks testing the current phase — I won't silently fix it *or* silently defer it;
   I flag it (*"this is a crash — fix now or defer to Phase N?"*) and you decide.

---

## 1. The MVP (the destination) — **SDF-only spine** (your call, 2026-06-20)

**MVP = a user can reliably do the full SDF object loop in Object Mode, with no crash, no invisible
object, no wedged GPU:**

> **Create** an SDF object → **edit** it (move/rotate/scale prims, change shapes) → **combine** it
> (union / subtract / intersect / smooth-blend) → **duplicate** it → **select / delete / undo** →
> **save → reload** and it's identical.

That spine, rock-solid, *is* the product's first usable version. Everything else is post-MVP.

**Explicitly OUT of MVP** (post-MVP, mapped in §4): voxel sculpting, polygon modeling, NURBS,
the AI/texture integration, the SdfObject (object→body→prim) hierarchy, advanced bevel
profiles/segments, import/export beyond the native format.

---

## 2. Tool & feature inventory (every tool — honest status)

Maturity: **works** (solid) · **partial** (works w/ gaps) · **broken** (present, failing) ·
**stub** (placeholder) · **missing**. Tier: **[MVP]** needed for the spine · **[post]** later.

### SDF modeling (the MVP surface)
| Tool / feature | Maturity | Tier | Note |
|---|---|---|---|
| Create SDF prim + shape library (box/sphere/cyl/…) | works | **[MVP]** | `creation.rs::create_prim` |
| Boolean ops: union / subtract / intersect | works | **[MVP]** | core CSG; gray-faces on cuts was a *render* gap (now fixed) |
| Smooth blend (smin / chamfer) | works | **[MVP]** | blend-K slider |
| Gizmo: move / rotate / scale (prim + body) | works | **[MVP]** | `gizmo.rs`, episode undo |
| Duplicate (Shift+D / Ctrl+D / menu / outliner / paste) | **partial** | **[MVP]** | bake+crash fixed; **still-invisible after bake** under diagnosis (GATE 1) |
| Delete / delete-last-object | works | **[MVP]** | freeze fixed (Object-Mode-Hybrid) |
| Undo / redo (transaction + episode) | works | **[MVP]** | `undo.rs`; verify across all spine ops |
| SDF prim bevel / rounding (`bevel_radius`) | works | **[MVP]** | field op, not poly bevel |
| Box face extrude / proportion edit (E tool) | **partial** | **[MVP]** | `face_ops.rs`; finish + verify |
| Mirror (`mirror_flags`) | works (verify) | [post] | nice-to-have for spine |
| Surface noise / modifiers | partial (verify) | [post] | |
| Blend falloff | works (verify) | [post] | |
| Per-prim + body materials | works | **[MVP]** | referenced by id; verify on DC mesh |
| Body transform (origin/rot/scale, no rebake) | works | **[MVP]** | body-local prims |
| Nesting / grouping / isolate | partial (verify) | [post] | |

### Object-Mode render & core systems (the MVP backbone)
| System | Maturity | Tier | Note |
|---|---|---|---|
| Object Mode = SDF baked→DC-mesh→raster (P0.2) | **partial** | **[MVP]** | gaps A/B/C/DUPCRASH fixed; full audit = GATE 2 |
| SDF Edit Mode (volume raymarch) | works (verify) | [post] | not required for the spine |
| Selection (unified) + GPU picking | works (verify) | **[MVP]** | must always work |
| Camera / viewport / grid / navigation | works | **[MVP]** | |
| Outliner (tree / rename / isolate / drag) | works (verify) | **[MVP]** | spine needs basic tree + rename |
| Properties panels (SDF) | works (verify) | **[MVP]** | every slider needs undo |
| Save / load (native project, v7) | works | **[MVP]** | v8 migration reverted; stabilize format |
| Lights | works (verify) | [post] | default light enough for MVP |
| Frame scheduler / pipeline warmup | works | **[MVP]** | |
| **Crash-safety (panic → clean exit)** | **fixed, NVIDIA verify pending** | **[MVP]** | S0.2: uncaught panic `process::exit`s before device/queue drop; atomic cache + reject |

### Voxel sculpt (post-MVP pillar)
| Tool | Maturity | Tier | Note |
|---|---|---|---|
| Sculpt mode entry/exit | works (verify) | [post] | ~0.5s entry lag noted |
| Brushes (add/sub/smooth/flatten/grab/…) | partial (verify) | [post] | GPU-authority atlas |
| Resolution / atlas / ≤512 raster vs >512 atlas | works (verify) | [post] | |
| Remesh (Ctrl+R) | works | [post] | 30s→5s done |
| Voxel→mesh display in Object Mode | works (verify) | [post] | VOX1024 fixed |

### Polygon / NURBS (post-MVP)
| Tool | Maturity | Tier | Note |
|---|---|---|---|
| Poly bevel (offset) | **stub** | [post] | `bevel_op.rs` scaffold + `BEVEL_PLAN.md`; v1 not wired |
| Poly extrude / knife / loop-cut / inset / subdiv | partial (verify) | [post] | `half_edge.rs` frozen |
| NURBS surface create/edit | partial (verify) | [post] | |
| Import / export (OBJ/mesh) | works (verify) | [post] | |

### AI / advanced (post-MVP)
| Feature | Maturity | Tier | Note |
|---|---|---|---|
| AI texture / NOVA integration | partial (verify) | [post] | gated on solid foundation per CLAUDE.md |
| SdfObject hierarchy (object→body→prim) | deferred | [post] | node type committed+dormant; resume whole |
| Advanced bevel (segments / superellipse / miter) | missing | [post] | bevel v2/v3 |

---

## 3. The road map to MVP

### Phase 0 — Foundation & dev-loop survival  ·  **← WE ARE HERE**  ·  size **S–M**
*Goal: a crash can't cost us a reboot, and we have a known-good checkpoint.*
- **0.1** Commit the 4 confirmed fixes (Bug A invisible-bake, Bug B gray faces, Bug C delete-freeze,
  DUPCRASH) as a checkpoint. **(S)**
- **0.2** Crash-survivability: a Rust panic must **fail clean** — release the GPU device, skip the
  cache write, exit — never wedge the driver / leak a process. **(M)** *Code landed on
  `fix/crash-safety`: uncaught panic exits before wgpu drop; cache write is temp+sync+rename
  and a bad file is rejected. Confirm on Windows/NVIDIA with `AZRA_TEST_PANIC=1`.*
- **Exit:** deliberate panic exits without a GPU wedge or reboot; checkpoint committed; app relaunches reliably.
- *(Detail: `STABILIZATION_PLAN.md` GATE 0.)*

### Phase 1 — SDF spine reliability  ·  size **M–L**
*Goal: the MVP spine works every time, across every entry point.*
- **1.1** Duplicate fully correct — visible, correctly placed, survives undo/redo + save/reload, from
  all entry points (Shift+D / Ctrl+D / menu / outliner / paste). **(S–M)** *(in progress: `[DUPDIAG]`.)*
- **1.2** Duplicate offset behaviour — decide on-top vs clear separation; make it intentional. **(S)**
- **1.3** Delete / clear — all cases incl. last object (done; verify in checklist). **(S)**
- **1.4** **P0.2 DC-mesh audit (one proactive pass)** — materials, normals, winding, invalidation on
  *every* mutation (add/delete/reparent/group/dissolve/undo/redo/load), asymmetric bodies,
  save/reload, selection/picking. Fix all found gaps in order. **(M–L)**
- **Exit:** the regression checklist (`STABILIZATION_PLAN.md` §6) passes for the full SDF spine, no
  crash / invisible / gray. Remove `[DUPDIAG]`.
- *(Detail: `STABILIZATION_PLAN.md` GATES 1–2.)*

### Phase 2 — SDF editing feels like a real modeler  ·  size **M**
*Goal: not just reliable — comfortable to actually build an SDF object.*
- **2.1** Finish box-face extrude / proportion editing (the E-tool) + verify the bevel cage follows. **(S–M)**
- **2.2** Confirm booleans / blend / mirror / falloff / materials are all solid end-to-end. **(M)**
- **2.3** Navigation/UX basics for modeling: snapping, focus/frame-selected, grid, pivots. **(S)**
- **Exit:** you can model a non-trivial SDF object (multi-prim, booleans, bevels, materials) comfortably.

### Phase 3 — MVP hardening → **MVP SHIPPED**  ·  size **M**
*Goal: trustworthy core.*
- **3.1** Stabilize + version the save format; autosave + crash recovery. **(M)**
- **3.2** Outliner / properties / selection polish for the SDF workflow (every control has undo). **(M)**
- **3.3** Performance pass — bake/DC latency during interaction within frame budget. **(M)**
- **Exit:** **MVP** — reliable SDF modeling, shippable. Sign-off here unlocks Phase 4+.

### Phase G — GPU mesh extraction (fluid editing)  ·  size **L** (its own sub-plan)  ·  **MVP (user call 2026-06-21)**
*Goal: heavy edits (body bevel, blend-K, boolean, large moves) update fluidly; the live-edit
"surface disappears" churn is closed structurally.* Full plan: **`GPU_MESH_EXTRACTION_PLAN.md`**.

- **The mechanism it fixes:** Object Mode meshes via ~125 ms CPU Dual Contouring on a background
  thread; `poll_dc_jobs` discards any DC result older than the current bake generation. During a
  continuous edit, each new bake supersedes the prior DC *before it lands* → surface stalls/vanishes.
  A ~2–5 ms GPU mesh finishes within the bake cadence → race closes. **Throughput fix, not a band-aid.**
- **What it does NOT fix** (and why it goes *after* the correctness gate): the recenter-shift /
  stale-region / invalidation-coherence bugs are **logic** errors — a faster mesher meshes the
  corrupted grid just as fast. **Stabilize those on CPU DC first (Phases 0–2).**
- **Reuse:** ~80% of the no-readback resident-pool infra already exists in the voxel-sculpt mesher
  (`voxel_gpu_mesh.rs`); the bake already produces a GPU-resident density buffer + texture. **One big
  new build:** a GPU Dual-Contouring **QEF** shader (Surface-Nets reuse would round off the very
  bevels/booleans this targets — the central risk).
- **Sub-phases:** **G0** offline parity harness (zero render-state — *parallelizable NOW*) → **G1**
  resident SDF mesh pool + `ResidentVolume.generation` → **G2** indirect draw behind a dev flag (CPU DC
  still authoritative) → **G3** generation-race closure + partial GPU re-mesh → **G4** cut-over (GPU
  default, CPU DC kept on-demand for save/export/fallback) → **G5** >256³ bodies (deferrable).
- **Exit:** continuous bevel/blend-K on a boolean body updates every frame with no disappear/stale
  frame; picking/selection + save/export/duplicate unaffected; CPU DC retained as fallback.
- **Sequencing:** **G0 may start in parallel now** (render-state-free, retires the shader-quality
  unknown); **G1–G4 land after the correctness gate (Phases 0–2)**; G5 optional post-MVP.

---

## 4. Post-MVP — feature tools (mapped now, built only after MVP sign-off)

> Order is a proposal; we re-confirm at MVP sign-off. Each becomes its own gated sub-plan.

- **Phase A — Voxel sculpt (2nd pillar)** · **L** — brush set, sculpt-entry lag, atlas scaling,
  Object-Mode coexistence with SDF. *(Mostly built; needs hardening + the SDF coexistence proven.)*
- **Phase B — Polygon modeling** · **L** — bevel v1 (offset chamfer, `BEVEL_PLAN.md`) → extrude /
  knife / inset / loop-cut, all app-layer (`half_edge.rs` frozen).
- **Phase C — SdfObject hierarchy (object→body→prim)** · **M–L** — finish as one whole feature
  (creation + outliner + selection + delete + materials + save migration).
- **Phase D — NURBS, import/export, advanced bevel (v2/v3 profiles/segments/miter)** · **L**.
- **Phase E — AI / NOVA integration** · **L** — gated on a solid foundation (CLAUDE.md).

---

## 5. You are here

- **Current phase: 0** (foundation). Active item: **0.1 commit checkpoint** + **1.1 duplicate
  diagnosis** (`[DUPDIAG]`, overlapping into Phase 1). The launch/GPU-wedge crisis is resolved (reboot
  reset the driver); the build with all four fixes runs.
- **Immediate next:** finish 1.1 (DUPDIAG result) → commit 0.1 → do 0.2 (crash-survivability).

---

## 6. Decision & divergence log (append-only)

- 2026-06-20 — Roadmap created. **MVP scope = tight SDF-only spine** (user). **Timeline = sequenced +
  S/M/L effort, no dates** (user). Voxel / poly / NURBS / AI / SdfObject = post-MVP.
- 2026-06-20 — Phase 0 elevated crash-survivability after a panic wedged the GPU and cost a full session.
- 2026-06-21 — **Scope decision (user):** GPU mesh extraction (was post-MVP perf, Option A) is **pulled
  into the MVP** as **Phase G** — "essential and part of the whole structure." Recorded as a conscious
  scope expansion (divergence protocol). Adversarial analysis: it **does** fix the live-edit churn
  (generation race vs ~125 ms CPU DC) but **does not** fix the recenter/stale-region/invalidation
  bugs → sequenced **after** the correctness gate, with **G0 (offline parity harness) parallelizable
  now**. Detail: `GPU_MESH_EXTRACTION_PLAN.md`.
- 2026-06-21 — **Brick Pool** (parked, 23 known bugs) was exposed as a one-click toggle in the normal
  properties panel; enabling it broke the production bake→DC→display (no live move/bevel updates).
  **Gated behind Developer Tweaks** + clean rebake on toggle. Defaults OFF (restart resets).
- 2026-06-22 — **Phase G progress (autonomous session, user asleep, authorized).** G0 (offline QEF-DC
  parity) + G1 core (resident + GPU indirect args) DONE + proven offline. Live readback path wired
  behind `gpu_sdf_mesh` (Developer Tweaks, OFF by default): GPU-mesh → mesh_cache → existing raster
  draw. Two drag-gates fixed (bake-suppress + readback-defer) so it meshes LIVE during edits, not just
  on release. Object Mode only. **Needs live user verification** (flag-gated; couldn't verify live).
  G3 (resident no-readback / indirect draw) deferred — device-loss-risky, needs live testing. Detail:
  `GPU_MESH_EXTRACTION_PLAN.md` Progress. App was left running so no fresh binary built — user rebuilds.
- 2026-06-21 — **PIVOT (user):** stop patching CPU-DC resolution/bake bugs (resolution→TextureOnly
  invisible-surface, "64/128/256 feel identical" = adaptive overriding the slider, bevel/blend live
  churn) — they are **artifacts of the CPU-DC meshing tech that Phase G replaces**, so fixing them is
  throwaway. **Phase G (GPU mesh extraction) is promoted to the ACTIVE work now**, ahead of the
  remaining CPU-DC stabilization (overrides the plan's "after the correctness gate" sequencing — a
  conscious divergence). **Caveat recorded:** GPU meshing does NOT fix the recenter-shift /
  stale-region / invalidation **logic** bugs (mutation/dirty layer) — those remain, addressed
  separately. The resolution clamp + brick-pool gate landed this session are kept as **stopgaps** to
  keep Object Mode usable during the transition. Start point: **G0** (offline GPU-DC parity harness)
  to retire the shader-quality unknown before touching the live render path.
