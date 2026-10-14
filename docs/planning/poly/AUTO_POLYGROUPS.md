# Auto-polygroups — the program records selections so the user never has to

**Date:** 2026-07-30 (revised same day)  
**Status:** building — storage + manual + select + overlay + bevel auto-tag  
**Owner:** *"store all selection to set a bit like polygroup… so if the user wants to reselect a
bevel later I could activate the polygroup visualisation, use a select polygroup… Selection sets
are powerful but a pain to manage — so let the program do it directly."*

**Verdict: yes.** Infrastructure already exists (`LayerStore`, `CustomData`, FL4 `PolyLayerRecord`).
The insight that makes this worth doing is the *automatic* part — every DCC has manual selection
sets and almost nobody maintains them.

---

## 1. Nothing like it existed — the container does

| Piece | Status | Where |
|---|---|---|
| Sparse typed-key attribute column | **built** | `LayerStore<K, V>` — `poly_mesh.rs` |
| Per-body runtime sidecar | **built** | `cache.poly_custom_data` |
| Persistence to `.fl4` | **built (v8+)** | `PolyLayerRecord` — append fields with `#[serde(default)]` |
| Propagation to new elements | **built** | `Interp::Copy` — for Euler ops that preserve IDs |
| Topology free-list survival | **built** | keyed by stable `FaceId`, not dense index |

**Persistence:** append `groups` + `group_meta` on `PolyLayerRecord` with `#[serde(default)]` —
forward/backward compatible, **no save-format version bump**, never touch `HalfEdgeMesh`.

---

## 2. Design (revised)

### Storage

```rust
/// Face → group id. Sparse: only *grouped* faces are stored. 0 is never stored (= ungrouped).
groups: LayerStore<FaceId, u16>,  // Interp::Copy for Euler ops

/// Per-body metadata (not per face). Index by `id` (1..); 0 reserved ungrouped.
group_meta: Vec<PolyGroupMeta>,

struct PolyGroupMeta {
    id: u16,
    name: String,
    color: [u8; 3],
    kind: PolyGroupKind,  // Manual | Bevel | Extrude | Inset | LoopCut | Bridge | Solidify | Other
    // Future reverse-op payload (unbevel):
    // bevel: Option<{ width, segments, shape, edge_count }>
}
```

**v1 membership:** one group per face (last write wins). Multi-group membership is a later phase.

### Auto-create: OPERATIONS, not every selection

| Op | Auto-group | Tag method |
|---|---|---|
| **Bevel** | `Bevel N` | **Post-rebuild:** faces after `new_face_start` (strips/caps/seals) — *not* pure Copy through `from_polygons` |
| Extrude / Inset / … | later | Euler path: `Interp::Copy` + mark new faces |
| Cancel / zero-width | **no group** | avoid ghosts |

**Manual hatch:** "Assign Selection to New Group" / "Assign to Group…" for everything auto misses.

### Bevel provenance (critical)

Bevel rebuilds via `from_polygons` → **all FaceIds change**. `Interp::Copy` alone cannot tag new strips.

**Rule:** `bevel_edges_offset` / `bevel_vertices_offset` record how many faces were **re-traced**
(STEP 3) vs **new** (strips + caps + seal). After install, faces with index ≥ `new_face_start`
receive the new group id. Re-traced surfaces keep (or lose) prior groups via fingerprint optional
later; v1 only tags new geometry.

### Empty purge + list hygiene

* **Always:** purge group entries with zero live faces after topology ops / on select UI open.
* **No hard recycle** of live groups. Soft hide empty/old only if list > ~32.
* **No auto-group** on cancelled drag, zero width, or rejected install.

### UI

* **Toggle polygroup visualisation** — tint faces by group colour (edit-mode overlay). Off by default.
* **Select ▸ Polygroup** in Tab menu — **only when groups exist** on the active poly body.
* **Select ▸ Polygroup under cursor** (optional, same menu) — ZBrush-style expand from hover/selected face.
* **Assign Selection to New Group** — Face mode.
* Later: group list panel (rename / recolour / delete / isolate).

### Unbevel relationship

Polygroups answer **which faces are Bevel N**.  
`PolyBevelHistory` still answers **params** (width/segments).  
Later: store params on `PolyGroupMeta.kind == Bevel` so reverse is a lookup, not a guess.

---

## 3. Gates

1. Save → load → group assignments + meta intact (empty default for old files).  
2. **Survival:** bevel → create group on new faces → extrude/edit elsewhere → select Bevel N still hits the same geometric region (face ids may rematch after purge of free slots; live faces with that id).  
3. Cancelled bevel → no new group.  
4. Cube 90° edges stay hard under auto-smooth; groups do not change shading.

---

## 4. Build order

1. ✅ Plan revised  
2. ✅ Storage + meta + PolyLayerRecord + save/load  
3. ✅ Manual assign + Select polygroup + overlay toggle  
4. ✅ Bevel auto-tag (`new_face_range` + confirm path)  
5. ✅ Empty purge + survival unit tests  
6. ✅ Extrude / Inset auto-tag on confirm  
7. ✅ Delete polygroup menu + Unbevel uses latest Bevel group when no selection  
8. ✅ Bevel params on group meta · rename UI · Solidify / Loop Cut whole-mesh groups  
9. Later: finer loop-cut face provenance · exact mesh restore without session history

---

## 5. Non-goals (v1)

* Multi-group membership per face  
* Edge groups (may derive boundary from face groups later)  
* Export to glTF face sets  
* Automatic coalesce of live groups  
* Paint / grow tools  
