# Untested-tools suite report (`AZRA_SOAK_SUITE=unt`)

**Commit:** `0ca1658`  
**Run:** `core_engine/soak_out_unt/`  
**Summary:** `OK=19 NO-OP=4 SLOW=3 FAIL=1 UNWIRED=0`

```
[SOAK] start — 27 steps suite=unt
```

---

## What was newly tested (previously not in soak)

| Step | Result | Notes |
|------|--------|--------|
| sdf torus / cone / capsule / stair / spiral_stair | **OK** | px deltas 0.5–1.9% |
| bool Intersect | **OK** | px 8.7% |
| create_op cycle | NO-OP px | action works (`Union→Subtract`) — tool state only |
| duplicate_selected | **OK** | prims 8→9, px 7.8% |
| set_resolution 256 | **OK** | vcap **2.0M → 16.5M**; bake `full 256³` |
| poly sphere | **FAIL** | **non-manifold on create** |
| poly cylinder | **OK** | |
| poly inset | **OK** | 2.7 ms |
| poly solidify | **OK** | 10 ms; overlay stall **221 ms** same frame |
| poly subdivide | **OK** | v 88→280, 14.8 ms |
| select all/invert/grow/shrink | **OK** | invert→0 when all selected (odd) |
| flip normals | **SLOW 148.6 ms** | STALL ~188 ms |
| recalc normals | NO-OP px | after flip, no further visual change |
| limited dissolve | **SLOW 134.4 ms** | faces 272→246 |
| poly join | **OK** | bodies 2→1 |
| poly fill | **OK** | delete face + fill 4 verts |
| convert_sdf_to_poly | **SLOW 1137.5 ms** | poly bodies 2→3; **~98k faces**; frame STALL **1843 ms** + TDR defer |
| convert_poly_to_sdf | NO-OP | async spawn only; not finished in settle |
| instance_create | **OK note but buggy** | prims **9→1**; WARN active_body invalid |

---

## Bugs / product issues for Claude

### FAIL-1 — Poly sphere mesh is non-manifold on create
```
FAIL:sphere topology: is_manifold: non-manifold: directed edge 7->8 used by 2 faces (same winding)
```
**Where:** `create_poly_mesh("sphere")` / poly mesh generators.  
**Impact:** Any topology op on default poly sphere starts from corrupted mesh.

### BUG-2 — Instance create collapses prim count + invalid body
```
[WARN] add_prim_to_graph: active_body_nid NodeId(1) invalid — recovering default body
prims 9→1  "instance did not add prim"
```
**Where:** `create_instance` / `add_prim_to_graph` after convert SDF→poly (body graph churn).  
**Impact:** Scene data loss / wrong target body after convert.

### PERF-3 — Convert SDF→poly is a multi-second hitch
```
action 1137.5ms
live_faces=98244  (256³ DC mesh)
STALL frame=1843ms  TDR defer
```
**Where:** `convert_sdf_to_poly` / materialize + HE build + GPU upload.  
**Impact:** UI freeze on “Convert to Mesh” for high-res SDF.

### PERF-4 — Flip normals / limited dissolve ~130–150 ms sync
```
flip_normals 148.6ms
limited_dissolve 134.4ms
```
**Where:** poly normals + dissolve ops + `poly_sync` GPU rebuild.  
**Impact:** freezes on modest meshes (~270 faces after solidify/subdiv — wait, after subdiv 272 faces / 280 verts; flip still 148ms is high for that size — maybe wrong nid or whole scene?).

Actually after solidify on box and subdiv, live_faces=272 — 148ms is still high for that.

### PERF-5 — Resolution 256 balloons capacity
```
vcap 2048384 → 16581375  (~16.5M)
VolumeUpload full (res=256³)
```
**Impact:** memory + bake cost cliff when user bumps resolution.

### ODD-6 — Invert selection from “all faces” → 0 selected
```
select_all=272 invert=0 grow=272 shrink=272
```
May be intentional (invert empty when everything selected with object mode?) — worth verifying.

### ASYNC-7 — convert_poly_to_sdf never settles in soak
Worker-thread convert; settle frames don’t wait for completion. Harness gap + possible user confusion (“clicked convert nothing happened yet”).

---

## Still not covered (honest backlog)

| Area | Why not in this suite |
|------|------------------------|
| Sculpt brushes (except Draw arm) | mostly `tw` stubs / need continuous stroke input |
| NURBS | all stubs |
| Knife freehand | mouse path |
| Box/circle/lasso select | screen-space input |
| Extrude/inset drag tools | mouse modal |
| SDF Emboss/Twist/Taper/… modifiers | need modstack API mapping |
| Spine / Draw Polyline | interactive |
| Array / Smart Mirror | incomplete |
| Export GLTF/OBJ dialogs | UI modal |
| PT long converge | separate harness |

---

## How to re-run

```powershell
cd core_engine
$env:AZRA_SOAK = "$PWD\soak_out_unt"
$env:AZRA_SOAK_SUITE = "unt"
$env:AZRA_UI = "v2"
.\target\debug\app.exe 2>&1 | Tee-Object soak_out_unt\console.txt
```

---

## Suggested fix order

1. **Poly sphere non-manifold** (correctness FAIL)  
2. **Instance + active_body after convert** (data corruption)  
3. **Convert SDF→poly async / progressive** (1843 ms stall)  
4. **Flip normals / dissolve perf**  
5. **256³ capacity / bake cost** UX warnings or progressive bake  
