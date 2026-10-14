# Resolution & Fidelity Roadmap — Review (2026-07-14)

**What this is:** owner-requested plan/review ("1024 feels good — what's the best plan to increase
resolution and fidelity from here"). Verified against master @ `711a7f9` by 3 code scouts
(caps/plan-items, mesh fidelity, bake/VRAM scaling). PLAN ONLY — no code changed.
**Companion:** `CACHE_REMESH_512_REVIEW.md` (the 512/1024 execution history; items #7/#10/#11 open).

---

## 1. Where we stand (verified on master)

| Thing | State |
|---|---|
| Object-Mode sparse ceiling | **1024³ live** (`max_sparse_resolution()=1024`), selectable in the Grid picker when the sparse toggle is ON. Dense ceiling 512 FOREVER. |
| Full 1024 rebake (measured, 4090) | classify **7.8 ms** (GPU) + finalize **87.7 ms** (CPU verdict path, worker) + bake **56 ms** (16 Z-slabs) + mesh **32 ms** ≈ **~184 ms**; 182k bricks / 355 MB pool / 1.46 M verts (sphere-subtract). Surface-proportional confirmed (3.91× bricks, 4.02× verts vs 512). |
| Drag feel at 1024 | Good by design: rigid-translate (no bake) → brick scroll → frozen-index partial ladder; windows measured 2–7 % of grid. |
| Mesher | **Real GPU QEF DC** (sharp features preserved; Tikhonov reg + centroid fallback, no SVD-rank step). Normals = crossing-plane average (crease-robust). Trilinear f32 sampling. |
| Memory | No dense res³ allocation remains on the sparse path. The legacy 812³ ≈ 2 GB VolumeAtlas was **stripped 2026-07-13** (old GPU logs showing it are stale). Everything big is surface-proportional: pool ~355 MB, cell_vert ~373 MB, mesh pool region ~80 MB @1024. |
| Open items from CACHE_REMESH plan | **#7** tiled *incremental* DC (pool/multi-draw landed; per-tile re-mesh still open — deliberately deferred: `[DcResidentSparse]` never fired >2 ms, trigger CPU dominated); **#10** Edit-Mode route-0 heal (NOT built); **#11** S2.6 structural sparse (toggle still the gate, default ON). |

## 2. Corrections to the "2048 walls" note (the pasted analysis)

Directionally right — **uniform 2048 is an LOD problem, not a dial** — but four specifics need fixing:

1. **"materialize fallback must be hard-disabled above 1024" — already structurally impossible.**
   No `materialize_grid_from_resident` exists; the dense seed path sits behind the ≥512 dense
   assert (`app_poll_bake.rs:243-250`, fails loudly) and sparse bakes carry no dense output at all.
   Nothing to disable.
2. **"50–100 M triangles at 2048" is overstated for measured scenes.** Measured 1.46 M verts @1024
   → ~6 M @2048 for the same scene (surface-proportional). Heavy scenes scale that up, but the
   real mesh walls are precise: (a) `cell_vert` **panics** (assert, `resident.rs:226`) near ~1 M
   bricks ≈ 2 GB; (b) the mesh pool's **1 GiB cap is self-imposed** — the 4090 allows 2.1 GB
   bindings; over-cap already degrades gracefully (per-body slot fallback). No "Phase-4 tiled
   multi-buffer pool" exists anywhere — that's an invented artifact; the graceful fallback is the
   current design.
3. **The biggest 2048 wall wasn't named: the bake dispatch is volume-proportional.** Mode-2 bakes
   sweep the full voxel box (1.07 B threads @1024 → 8.6 B @2048, 32 blocking Z-slab polls); the
   sentinel early-out is per-thread, not per-dispatch. Everything else went surface-proportional —
   this is the one leg still scaling with res³.
4. **"one 2048 body survivable, two blows the pool" — partially wrong mechanism.** The atlas LRU
   (25 % VRAM, 2 GiB cap) governs only brick pools, and the in-use body is never evicted; but
   `cell_vert` + mesh regions live **outside any budget**. There is **no global VRAM ledger** —
   that, not the pool cap, is the multi-body oversubscription gap.

## 3. The plan (staged; fidelity-first, since 1024 already feels good)

### Stage 0 — free wins & safety rails (days, do first)
- **S0.1 ~~Engage GPU-finalize~~ — RETRACTED after code check (2026-07-14).** Production ALREADY
  runs STAGE-3: the lazy compile builds classify+finalize as a set (`gpu_init.rs:365-368`) and the
  async hot-swap delivers them atomically (`PipelineReady::ClassifyBricks(p, f)`,
  `app_poll_pipeline.rs:97-103`) — `gpu_finalized` is always true when a GPU exists. The doc's
  "finalize 87.7 ms" is the hardware test's frame-thread **decode/validate of the GPU-finalized
  index** (linear 2.1M-u32 scan, `tests_sparse_bake.rs:1591-1595`) measured in a **debug build**;
  release cost is a fraction. **Residual CLOSED (2026-07-14, owner's in-app S2.6 test):** a live
  1024 bake logs `validate=5.6–6.7 ms` across ~14 bakes — and that's a **debug** build, so release
  is a fraction of it. Below the "optimize only if >5 ms" bar in the regime that matters. **S0.1 is
  fully done; no work remains.** (Same log: classify 101.9 ms cold → **9.6–17.9 ms** warm.)
- **S0.2 S2.6 structural sparse (#11).** Kill the dev toggle as the gate; ≥512 = sparse, period.
  Removes the "1024 needs Developer Tweaks" UX wart and the R1 regression class.
- **S0.3 Global VRAM ledger + graceful cell_vert.** Sum {brick pools, cell_vert, mesh regions,
  slots, big textures} against `vram_bytes`; demote per-body resolution (the adaptive machinery
  already exists) instead of oversubscribing; convert the cell_vert assert into degrade. Closes
  the remaining device-loss class before anything gets bigger.
  **THE RATCHET IS FIXED — `35a39fe` (2026-07-14). The ledger item itself REMAINS OPEN.**

  *Evidence that started it (owner's in-app 1024 drag):* the mesh pool grew
  `verts 262→393→590→885 MB` + `idx 197→295→443→664 MB` across four `[SdfPool] rebuild` gens —
  exactly 1.5× each, monotone — while the brick count grew only **+23 %** (170 784 → 210 612).

  *My first diagnosis was WRONG* (the 6th error of the day) and is recorded here because the
  correction is the useful part. I claimed a "1.5×-**on-overflow** ratchet" with capacity "decoupled
  from need", computed "162 verts/brick ≈ 8× the measured 20", and suspected spurious overflow
  detection. All false: `[SdfPool] rebuild` prints **`new_vbytes` — the BUFFER**, not the slot cap.
  Dividing the *buffer* by bricks is meaningless. The **slot cap held at exactly 48.0 verts/brick at
  all four gens** (= `VERTS_PER_BRICK_HEADROOM`, which never misbehaved), **no overflow ever fired,
  and `grow_slot` never ran.** The log carries *zero* information about the real mesh — no counter
  value is ever printed.

  *Actual root cause — `pool.rs` `rebuild()` sized the new buffer from the pool's OWN previous size:*
  `buffer(n+1) = max(need, 1.5 × buffer(n))` — a **recurrence on itself**. `need` tracks the surface
  (≈ +12 %/step) while the multiplier is a flat +50 %, so `1.5 × buffer` outran `need` at the first
  rebuild and the `need` term **could never win again**; each size became the next base. Growth was
  driven by ordinary brick accumulation through `dc_output_caps_sparse(bricks, 48)` → `ensure_slot`
  → rebuild. Reproduced **8/8 exactly, to the printed 0.1 MB**, with zero free parameters — and
  confirmed independently by the log's **consecutive generations 1,2,3,4**: `pool.rs` bumps the
  generation on bump-append relocation too, so no gap ⇒ the fast path never fired ⇒ every grow
  rebuilt ⇒ every rebuild ratcheted.

  *Fix (2 lines):* anchor headroom to **demand** — `(need_v * 3 / 2).min(safe_max).max(need_v)`. The
  output no longer re-enters the input, so it cannot compound: `buffer ≤ 1.5 × Σ live regions`
  always, and a stable surface reaches a **fixed point**. On the logged sequence: **849 MB instead of
  1 549 MB**, and bounded rather than climbing to `safe_max`. Gated by
  `sdf_pool_rebuild_sizing_tracks_demand_not_history` — **proven to FAIL on the old code** (step 2:
  buffer 5 899 392 > bound 4 746 240) and pass on the fix.

  **STILL OPEN for the real S0.3 (Fable):** (a) **no global VRAM ledger** — mesh pool regions,
  `cell_vert`, and per-body slots live outside the atlas LRU's 25 %/2 GiB budget, which governs only
  brick pools; (b) the `cell_vert` **assert** (`resident.rs:226`) must be prevented structurally —
  CLAUDE.md forbids downgrading it to a log, so the fix is to demote resolution *before* allocating;
  (c) **no shrink / free-on-unpool** — the pool is still grow-only for a body's lifetime and a
  deleted body's region is reclaimed only by the next rebuild; (d) **no region free-list**, so the
  bump-append fast path stays dead for a single-slot pool (frequency only — it clocks rebuilds, it
  no longer compounds them). §1's documented "mesh pool region ~80 MB @1024" is still **~4× low**
  even post-fix (~324 MB of true demand for that body).

### Stage 1 — fidelity at current resolution (best value per effort)
- **S1.1 ~~Edit Mode stops downgrading to dense 512~~ — RETRACTED, premise false (2026-07-14).**
  Found while doing S0.2 + confirmed by adversarial review: **Edit Mode does NOT downgrade.**
  `gridbacked_only` is true **from the first bake onward** — `dirty.rs:164` sets it unconditionally
  at the top of `trigger_body_vol_bake` (its only non-test writer), and the `force_volume_only` flag
  that could clear it was DELETED 2026-07-13 ("provably constant false"). *(Precision fix 2026-07-14,
  found during S26-1: the field **defaults FALSE** at `mod.rs:584`, so "ALWAYS true" — my original
  wording — is wrong for the startup window before any bake. Nothing here changes: S2.6's gate is
  only read during bake dispatch, after `trigger_body_vol_bake` has set it, and the removed
  `sparse_bake_debug` defaulted true so `X && gridbacked_only ≡ gridbacked_only` exactly. But it
  DOES mean `max_mesh_resolution()` must never drive a UI ceiling — the 1024³ chip would flicker
  during startup. That is why S26-1's picker keys on its own route, not on the cache.)* The tree
  states it outright:
  *"gridbacked_only stays TRUE even in 'SDF Edit' / WorkMode::Vertex"*. So a 1024 body keeps its
  sparse DC mesh in Edit Mode; the C5/TextureOnly-clamp downgrade and the 0.7–1.0 s B3 entry freeze
  are **historical** (they died with the legacy volume-raymarch display). The scout report that
  claimed otherwise read the dead branches as live. **Consequences:** (a) #10 "route-0 heal" is
  moot for this reason — re-verify it has any remaining purpose before scheduling; (b) the
  `gridbacked_only == false` branches (`max_gridbacked_resolution` arm of `max_mesh_resolution`,
  the TextureOnly clamp in `effective_resolution`, the conjunct in `sparse_bake_eligible`) are DEAD
  — retained as the structural guard if an Edit-Mode dense regime ever returns. The live dense
  regime is `res < 512` only.
- **S1.2 ~~Edge-crossing refinement~~ — RETRACTED, mathematically impossible (2026-07-14).**
  `sdf_mesh.wgsl:245` already computes `tt = da/(da-db)` — the **exact analytic root of the
  trilinear field along that edge**. A trilinear field restricted to an axis-aligned lattice edge
  IS linear, so bisecting *that same field* provably converges to the identical point: the change
  yields **byte-identical geometry at strictly higher cost**. There is no salvageable version of
  "bisect the same field". (My "~zero memory, small shader cost, cheapest lever available" framing
  was wrong at the premise, not the estimate.) Sharp features are already recovered from
  gradient-normal QEF planes (`:250`, `:264-303`), not from root precision — **the accuracy ceiling
  is the BAKED GRID, not the root-finder.** The only real version is a NEW, different item:
  root-find against the **analytical** SDF (`eval_prim`) instead of the interpolant — which needs
  the prim buffer + the whole `sdf_prim_eval.wgsl` chain bound into `sdf_mesh.wgsl` (today: 8
  grid-only bindings) plus a full analytical eval per crossing (~3–12/cell). That is a **large**
  item, not a cheap one. Do not schedule it as "S1.2".
- **S1.3 ~~Normals consistency~~ — RETRACTED as framed (2026-07-14).** The mechanical claim is true
  (only the CPU path reads `smooth_dc_normals`; the GPU DC ignores it) but **"silent divergence
  today" is FALSE**: the CPU mesher is a Developer-Tweaks fallback requiring BOTH `gpu_sdf_mesh`
  and `gpu_sdf_mesh_resident` to be manually unchecked (both default true). In production the two
  paths never both run, so the toggle is an **inert no-op**, not a divergence. **Do NOT "wire it
  into the GPU DC"** — that would reintroduce the gradient-normal sampling `sdf_mesh.wgsl:296-303`
  documents as the deliberately-rejected old approach (jagged/eroded crater rims along cuts). Real
  work = the *remove* half only: delete or relabel the inert "Smooth Cache Normals" control
  (`properties_render.rs:86-93`). **Opus, trivial, cosmetic** — not a fidelity item at all.
- **S1.4 Continuous subdivision rounding.** The per-prim "subdivision" is a 6-step lookup table
  (−0.18…−0.003). Make it a continuous radius so rounding scales with intent, not quantized steps.
- **S1.5 (evaluate, don't assume) SVD/rank-truncated QEF** for thin/degenerate features — only if
  S1.2 leaves visible artifacts on thin walls; otherwise skip (current det-fallback is adequate).

### Stage 2 — make 1024 the default *feel* (multi-body, stalls)
- **S2.1 Re-measure, then decide #7 (incremental tiled DC) — RE-MEASURED 2026-07-14: still DON'T
  build it.** Owner's live 1024 drag: the mesh term never appeared as a spike; the cost is
  `[SparseEscalate]` firing **15+ consecutive times** ("window >1/8 of the grid"), each forcing a
  full classify + full 1024³ bake ≈ **19–21 ms** `poll_background_tasks`. The window grew
  monotonically (`[246,245,246]..[783,778,778]` → `[0,403,231]..[434,965,793]`) — i.e. a drag that
  wanders trips the 1/8 heuristic and never returns to the cheap partial ladder. **#7 would optimize
  the wrong term.** The real S2.x question is whether the escalate heuristic should re-tighten its
  window once the prim settles (owner reports the drag "felt good" on a 4090 debug build, so this is
  a scaling risk, not a felt bug — do not chase it reactively).
- **S2.2 Full-rebake UX at 1024.** After S0.1 the stall is ~96 ms (bake 56 + mesh 32 + classify 8).
  If still felt on scope-leave/boolean edits: overlap classify→bake→mesh across frames (they're
  already async-capable) rather than draft-cascading (sparse deliberately never drafts).
- **S2.3 Screen-space-error resolution.** Upgrade the camera-LOD tiers (0.15/0.05/0.01 screen
  fraction) to a voxels-per-pixel target so per-body res follows what the eye can see. This is
  also the foundation Stage 3 needs.

### Stage 3 — beyond 1024 (2048 is adaptive, not a cap bump)
Agreed with the pasted note's conclusion, with a sharper decomposition:
- **S3.1 Brick-list-driven bake dispatch** (the real enabler): dispatch mode-2 bakes over the
  *surface brick list* (indirect dispatch, one workgroup per brick octant) instead of the full
  voxel box. Turns the last res³ leg surface-proportional and moots the 32-slab TDR serialization.
  Well-defined, testable against the existing oracle-parity gates.
- **S3.2 Selective per-body 2048.** Key insight the flat architecture already supports: **res caps
  are per-body, and cost is surface-proportional — a small/medium body at 2048 is cheap** (its
  brick count, not the scene's, is what scales). Allow 2048 per body behind a classify-first
  budget check (estimated bricks × 2 KB within ledger headroom → permit; else clamp + toast).
  Detail where the artist asks for it, no octree required.
- **S3.3 Mixed-resolution bricks (octree-lite, 2-level)** — only if S3.2 proves insufficient for
  *large* hero bodies: fine bricks in dirty/near-camera regions, coarse elsewhere. This is the
  genuinely big feature (classifier, index, mesher seams all touched); do not start it before
  S3.1/S3.2 exist and a real scene demands it.
- Rasterization of sub-pixel triangles at high res is answered by S2.3 (bodies only reach 2048
  when their voxels are ≳ pixel-sized) — not by mesh decimation.

## 4. Recommended order (one line)
**S0.2 ✅ → S1.3 → S1.4 → S1.2 → S0.3 → S2.3 → (re-measure: S2.1/S2.2) → S3.1 → S3.2 → (only
if demanded) S3.3.** (S0.1 AND S1.1 both retracted — each was already true / already dead; see
above. S0.2 shipped 2026-07-14.)

Rationale: two of the three "Stage 0/1 headline" items evaporated on contact with the code (S0.1
already engaged; S1.1's downgrade died with the legacy volume display), which is itself the
finding: **1024 is in better shape than the docs implied.** What's left that genuinely moves the
needle is fidelity-per-voxel (S1.2 edge bisection — cheaper than any resolution increase) and,
before pushing further, the VRAM ledger (S0.3 — still the real multi-body oversubscription gap).
2048 becomes a contained, per-body feature (S3.2) once the bake is surface-proportional (S3.1) —
the octree is a last resort, not the plan.

### Lesson recorded (2026-07-14) — **FIVE of this roadmap's items were wrong on first write**
A premise-verification pass (7 items × 1 verifier + 2 adversarial skeptics, every claim forced to
cite the **writer**) retracted five of my own items. The failure modes were NOT all the same, which
is the point:

| Item | I claimed | Truth | Failure mode |
|---|---|---|---|
| S0.1 | GPU-finalize not engaged | always engaged | read a dead branch as live |
| S1.1 | Edit Mode drops to dense 512 | `gridbacked_only` has no false-writer | read a dead branch as live |
| S1.2 | bisection = cheap fidelity win | provably **byte-identical output** | **never checked the math** |
| S1.3 | silent CPU/GPU divergence | CPU path unreachable by default | mistook a dev fallback for production |
| escalate | window accumulates, never resets | `dirty_regions.remove()` runs every dispatch | **my own log disproved it** |

The escalate one is the most instructive: I *quoted* the window shrinking on x (`783 → 434`) in the
same breath as calling it "monotonic". Accumulation cannot shrink. The data to falsify my claim was
in the sentence I wrote it in.

**Rules for this roadmap, earned the hard way:**
1. **Verify the premise at the WRITER** — who sets the flag, unconditionally? No reachable writer =
   a guard, not a feature. (Catches S0.1, S1.1.)
2. **A default-OFF or dev-gated path is not "production".** Check the default before calling
   anything a live divergence. (Catches S1.3, and re-scopes S2.3.)
3. **Do the arithmetic before proposing a numerical improvement.** (Catches S1.2.)
4. **Re-read your own evidence against your own claim.** (Catches escalate.)
5. Items here are **hypotheses until verified**, and a scout's report is not verification.

### Task split — VERIFIED 2026-07-14 (supersedes the guess below it)
Every row below was verified at the writer and adversarially challenged. **My original split was
mostly wrong**: I had sent Fable-tier work to Opus (S1.4) and Opus-tier work nowhere (S1.3), while
three of the "Opus easy wins" turned out not to exist.

| Item | Verdict | Tier | Size |
|---|---|---|---|
| **S0.2** S2.6 structural sparse | ✅ **SHIPPED** `9ef06bc` | opus | done |
| **S0.1** GPU-finalize | ✅ **CLOSED** — was already true; residual answered by live log (`validate=5.6–6.7 ms` in DEBUG) | — | 0 |
| **S1.1** Edit-Mode downgrade | ❌ **RETRACTED** — no false-writer | — | 0 |
| **S1.2** edge bisection | ❌ **RETRACTED** — byte-identical output, provably | — | 0 |
| **S1.3** normals | ❌ retracted as framed → **delete the inert toggle** | **opus** | trivial, cosmetic |
| **S26-1** picker OOM | ✅ **REAL, reachable in one click** — 4.29 GB CPU `Vec<f32>` on the UI thread → abort | **opus** | small, contained |
| **S1.4** continuous rounding | ✅ real, but **retiered opus→FABLE** | **fable** | hard, ~90–130 LOC / 9 files |
| **S0.3** VRAM ledger | ✅ real | **fable** | hard |
| **escalate 1/8** | ❌ premise retracted → real defect is the *threshold*, and it is a **device-loss guard**, not a perf knob | **fable** | moderate |
| **S2.3** SSE resolution | ✅ real but **misdirected** — flag ships **OFF**; two rival formulas | **fable** | hard, ~250–350 LOC |
| **S03-1** pool ratchet | ✅ **SHIPPED** `35a39fe` — rebuild sizing anchored to demand, not history | opus | done |
| **S26-1** picker OOM | ✅ **SHIPPED** `3f6b092` — `GridRoute` derives ceiling + click from one value | opus | done |
| **LSR** legacy sculpt removal | 🆕 **owner-authorized, plan written, C0 shipped** `e9fa024` → `LEGACY_SCULPT_REMOVAL_PLAN.md` | opus C1–C3/C5, **fable C6** | multi-session |
| **S26-2** loader unclamped | 🆕 open — a pre-fix `.fl4` at 1024 allocates 4.29 GB on **every load** | opus | small |
| **S26-3** dead GPU clamp | 🆕 open — clamp never fires + `fog_bg` invalidation + frame-ordering (C3 closes the last part) | opus | small |

**Why S1.4 is Fable, not Opus (my error):** it retypes a **serialized bincode field**
(`SdfPrimitive.subdivision: u32` → f32 radius). Width is identical (bincode-1 fixint, 4 bytes) so
there's no field cascade — but a wrong load-time migration **silently** changes the look of every
existing `.fl4` with subdivision>0. Serialized-shape changes are automatically Fable.
**Bonus finding (Tier A, do first, independent):** the **CPU evaluator ignores `subdivision`
entirely** (`sdf_math.rs` `fold_prim`/`world_distance` — zero hits) → an *existing* violation of
"Feature Must Work in Both Paths". That's a real parity bug hiding under a cosmetic item.

**Why the escalate threshold is Fable:** `bake_dispatch.rs:1040-1044` documents the 1/8 constant as
a **device-loss guard** — the frame-thread sparse walk balloons the brick pool → "Command encoder is
invalid" → device loss. Lowering it to "make partials eligible" re-exposes that exact class. The
real defect is that the threshold is **geometrically unreachable for a dominant prim** (grid = 1.3×
the padded prim union ⇒ a single-prim body's window is ~45 % of grid volume vs a 12.5 % threshold),
so the cheap partial ladder is effectively dead for single-prim bodies — but owner reports no felt
issue, so **confirm a symptom before investing**.

**Correction to S2.3's premise:** the tiers live in `update_lod_resolutions` (mod.rs:973-1067),
which IS live and maintained. The build warning about `update_body_adaptive_resolution` being unused
is a **red herring** — that function is genuinely dead but contains no tiers; delete it as unrelated
cleanup. The real blocker is that `use_adaptive_resolution` **defaults to false** (mod.rs:609) and
two rival formulas write the same map.

<details><summary>Original (mostly wrong) guess, kept for the record</summary>

- **Opus 4.8:** S0.2, S1.3, S1.4, S1.2, S2.3 formula. Optional: S0.1 residual.
- **Fable:** S1.1, S0.3, S2.1/S2.2, S3.1, S3.2, S3.3.
</details>
