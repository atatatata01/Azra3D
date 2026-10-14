# Sidecar survives a boolean — plan (owner-approved 2026-08-04: "YES")

One job, two payoffs the owner cares about:
1. **UV / seam / sharp / crease / vcolor / polygroups stop being destroyed by every boolean.**
2. **Bevel Smart Select gets persisted tags** — the thing his feature actually needs
   (`BEVEL.md` §6 "DESIGN REDIRECT": tags must be written when the OPERATION runs and stored
   with the mesh, not threaded through one modifier evaluation).

## What is destroyed today, and where

`CustomData` (`core_math/src/poly_mesh.rs:645`) is the runtime sidecar:

| layer | key | domain |
|---|---|---|
| `uv` | `HalfEdgeId` | corner |
| `seam`, `sharp` | `HalfEdgeId` (BitLayer) | corner |
| `crease` | `EdgeKey` | edge (twin-symmetric) |
| `vcolor`, `vcrease` | `VertexId` | vertex |
| `groups` (+ `group_meta`) | `FaceId` | face |
| `extra` | `LayerKey` | any |

Every boolean/extrude install CLEARS it, because the ids it is keyed by are meaningless after
the kernel renumbers: `app/src/poly/boolean.rs:549-551` and `:562` (the `BOOL-STALECD`
comment), `poly_drag_tools/extrude_drag.rs:1397-1399`, `poly/mesh_sync.rs:656`.
`mesh_boolean_manifold.rs:14` already flags this as a known gap.

## The remap

Ids do not survive; POSITIONS do — for geometry the boolean did not cut, which is precisely
the geometry whose data we must keep (owner's standing rule: *a boolean must not change
geometry it did not cut*). Use the same quantized-position keying the boolean already trusts
(`PRESERVE_Q = 1e-5`, `mesh_boolean/rebuild.rs:150`), so the remap and the kernel agree by
construction.

```
CustomData::remap_by_position(&self, old: &HalfEdgeMesh, new: &HalfEdgeMesh) -> CustomData
```

- **vertex layers** — map `quantized position -> old VertexId`; for each new vertex, copy when
  its position matches. New vertices get the layer default.
- **edge layer (`crease`)** — key on the unordered quantized endpoint PAIR.
- **corner layers (`uv`, `seam`, `sharp`)** — key on `(face centroid, corner position)`: a
  corner is identified by which face it belongs to and where it sits, both stable for uncut
  geometry.
- **face layer (`groups`)** — key on quantized centroid. Faces the boolean SPLIT get no group
  (a split face is genuinely new); `group_meta` carries over wholesale since ids are per-body.
- Anything unmatched keeps the default. **Fail closed: never guess a value onto new geometry.**

## Then the tags (what Smart Select consumes)

Add ONE new bit layer, `HalfEdgeId` domain, alongside `seam`/`sharp`:

```
/// Edge was CREATED by an operation (bevel segment, boolean seam) rather than authored.
pub op_created: BitLayer<HalfEdgeId>,
```

- The **boolean** sets it from `BoolProvenance` (already built + green, commit 0753564):
  `edge_is_new` marks exactly the created edges, and `new_flat_edges` the flat subset.
- The **destructive bevel tool** sets it for the strip/cap/seal edges it emits
  (`bevel_op::last_bevel_new_face_range` indexes the installed mesh on the full-rebuild path —
  see `BEVEL.md` §6 for the one path where it does not).
- **Smart Select** reads it: skip edges with `op_created`. No inference, no angle, no
  provenance threading — the tag was written when the operation ran, which is what the owner
  specified.

## Staging

**S1** `remap_by_position` + tests: cube with UVs/creases/groups, subtract a cylinder, assert
the UNTOUCHED faces keep every layer and the cut region carries defaults. No call sites yet.
**S2** Call it at the boolean install in place of the `BOOL-STALECD` clear; keep the clear as
the fallback when remap is unavailable (BSP path / no provenance). Ship — this alone stops the
data loss.
**S3** Add `op_created`; the boolean writes it from `BoolProvenance`.
**S4** The destructive bevel writes it.
**S5** Smart Select reads it; delete the in-stack provenance path in `mesh_sync.rs`.

## RESOLVED 2026-08-11 — tags persist via the `FL4PTAGS` TAIL SECTION, and the real loss was the INSTALLS

Two separate things were wrong; the section below (kept for its dead ends) diagnosed only one.

**1. The in-session loss was never the remap.** Measured before touching anything:
`remap_by_position` carries `op_created` across a boolean with ZERO loss on geometry the cut did not
reach (gate `op_created_survives_a_boolean_on_untouched_geometry`, `mesh_boolean/tests.rs`). The
tags died at the INSTALL. Six ops replaced `PolygonalBody.mesh` with a renumbered rebuild while
leaving `cache.poly_custom_data` keyed by the old ids — **Boolean Cut** (the owner's op), Knife Cut,
Knife Slice, Join, Add-to-Mesh, and Apply-modifier. Not a clear: the surviving entries name unrelated
edges of the new mesh. Fixed structurally with ONE install entry point,
`AzraApp::poly_install_rebuilt_mesh` (`poly/mesh_sync.rs`), which installs the mesh and re-keys the
sidecar in the same step. Any new destructive op must go through it — a hand-rolled
`*mesh = Arc::new(..)` is how all six of these happened.

**2. Persistence: option (1)/(2)/(3) below were a false trilemma.** The section concluded the only
ways left were a version bump, re-deriving the tags from geometry, or giving up. It was written
before the FL4 tail-section pattern existed; there are now fourteen such sections. `FL4PTAGS`
(`PolyOpTagRecord` = node_id + half-edge ids, written last, emitted only when a body carries a tag)
costs **zero blob bytes**, so an untagged scene saves byte-identically to a pre-feature file and no
old `.fl4` can be affected — which is precisely what both earlier attempts got wrong by putting the
data IN the blob. Gate: `poly_op_tags_round_trip` (`app/src/project/tests.rs`).

**The lesson worth keeping from below:** the failure was never "which struct" — it was IN-BLOB vs
OUT-OF-BLOB. Anything new that must persist goes out of blob.

## OPEN (HISTORICAL — superseded by the section above; kept for the two dead ends) — the tags DO NOT PERSIST YET, and the obvious fix corrupts old files (2026-08-05)

The owner's `TEST_06.fl4` exposed this: `op_created` lives in the RUNTIME sidecar only, so a
save/reload loses it and Smart Select silently has nothing to skip. Authored layers persist
via `PolyLayerRecord` (`app/src/project/mod.rs:297`), so adding an `op_created: Vec<u32>`
field there is the obvious move.

**It was tried and REVERTED: it breaks every existing `.fl4`.** `PolyLayerRecord` is
**bincode**-serialized, and bincode is NOT self-describing — `#[serde(default)]` does nothing
for a missing trailing field, so an old file's byte layout no longer matches the struct and
the whole side-channel fails to deserialize. `intrude_owner_fixtures_pinned` caught it
immediately (the owner's own scenes stopped loading). This is precisely the trap recorded in
CLAUDE.md's "Serialized enums / bincode" rule and the `project_bincode_save_format_field_trap`
memory — I walked into it anyway.

**AND THE "CORRECT SHAPE" ALSO FAILS — MEASURED (2026-08-05).** The second attempt did
exactly what this file and `project/mod.rs:41-48` prescribe: a separate `PolyOpTagRecord`
keyed by `node_id`, as its own trailing `#[serde(default)] Vec` on the blob struct, the
`voxel_grids` precedent. It breaks old files too:

```
load_binary: "Deserialize error: invalid u8 while decoding bool, expected 0 or 1, found 111"
```

**So the in-code comment claiming "a trailing `#[serde(default)]` field on a STRUCT is
forward/backward-compatible" is WRONG for bincode.** Bincode is a positional format with no
field tags: `serde(default)` only helps when the *reader* is given fewer bytes AND the format
can tell that the stream ended — inside a larger blob it simply keeps reading the next
field's bytes as this one. `voxel_grids` presumably worked because it landed with a format
change, not because the pattern is safe. **Do not trust that comment; it will cost the next
person the same two attempts.**

**What actually remains, none of them free:**
1. **Version bump + migration** — read old files with the old struct, new with the new. The
   honest fix, but `.fl4` version changes are owner-visible and bound by CLAUDE.md's
   save-format lockstep rule (both branches, same session).
2. **Derive the tags on load** instead of persisting them: re-run the classification from
   geometry when a body is loaded. Cheap, but it is inference — which the owner explicitly
   rejected for Smart Select.
3. **Keep tags session-only** and document it: everything works until you save. That is
   today's behaviour.

This needs an owner decision, not another attempt. Verify ANY future attempt against
`intrude_owner_fixtures_pinned` — it caught both failures immediately.

## Invariants

- `HalfEdgeMesh`'s serialized shape is untouched — the sidecar is runtime state
  (`POLY_FOUNDATION_PLAN` HEX-BL rule); authored layers persist via the existing in-blob
  `Vec<PolyLayerRecord>` on `ProjectFile`, never a new trailing magic.
- Remap is ADVISORY: absent/failed remap falls back to today's clear. Never worse than now.
- A tag must never cause geometry to be silently skipped when its provenance is unknown —
  Smart Select stays fail-closed (bevel everything).
- `cargo test -p core_math` + `-p app` green per stage.
