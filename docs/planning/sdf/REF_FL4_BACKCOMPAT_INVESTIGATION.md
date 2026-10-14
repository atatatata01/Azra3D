# `dc_gate0_committed_ref_fl4_loads_unchanged` — investigation (UNRESOLVED)

**Date:** 2026-07-29
**Status:** root cause NOT yet found. This records what is proven so the next attempt does not
repeat the search.

## What it is

A backward-compat gate: the committed `docs/planning/sdf/bench/dc_consolidation_ref.fl4`
(written 2026-07-17, `7437432`) must still load. It currently fails with:

```
Deserialize error: invalid u8 while decoding bool, expected 0 or 1, found 63
```

Until `1067376` this test failed at `deserialize_any` instead — the `BlendCurve` bug was masking
this one. Fixing that revealed this. **It is older and independent, not a regression from it.**

## Proven facts (measured, not inferred)

* **Bisect: first bad commit is `47007be`** ("feat(modifiers): start stack plan"), which added
  `ProjectFile.body_mesh_modifiers`. Verified green at `47007be~1`, red at `47007be`.
* **The error CHANGED since then.** At `47007be` it is `io error: failed to fill whole buffer`
  (ran off the end). Today it is the bool error. So there are **at least two** distinct breaks
  stacked, and the bisect result only identifies the first.
* **The ref file has NO FL4 tail sections** (`FL4ORDER`/`FL4MATX2`/… all absent). It is a bare
  17295-byte blob that simply ends. Version is 7 — the same version current files write, so
  **version cannot disambiguate old from new.**
* **`#[serde(default)]` is INERT in bincode.** There are no field names in the stream, so serde
  never learns a field is absent; it just keeps consuming. Proven with a standalone probe: a blob
  written without a trailing field fails to read into a struct that has one, `default`
  notwithstanding. The claim in `project.rs` that "a trailing `#[serde(default)]` field on a
  STRUCT is forward/backward-compatible" **is false**, and several fields were added on that
  belief.
* **Appending to a NESTED struct also breaks the wire.** Proven with a probe: appending a field to
  a struct held in a `Vec` inside an outer struct makes every later outer field misread. This
  matters because `SdfPrimitive` is nested inside `SdfBody` — so "append only" does NOT make it
  safe to add a field to `SdfPrimitive`.
* **`SdfPrimitive` had a genuine mid-struct INSERTION**: `blend_profile_shape` and `blend_curve`
  were inserted between `blend_profile` and `node_graph` (index 21 of 43), not appended. That is a
  real bug on its own. **But fixing it — moving both to the end, and separately making both
  `#[serde(skip)]` — did NOT fix this test.** So it is not the (only) cause.

## Ruled out (diffed old vs new, no wire-affecting change)

`SceneNode`, `SceneGraph`, `NodePayload` variant order, `SdfBody` (the two added fields are
`#[serde(skip)]` and follow the already-skipped `child_order`), `LightData` (pure append, both
skipped), `PbrMaterial` (pure append, all six skipped), `TexMap` (`absolute`/`blend` appended;
skipping them changes nothing here), `ModifierType` and `BlendProfile` (both append-only),
`SceneData` / `CameraState` / `RenderSettings` / `EditorSettings` (all unchanged).

## The decisive measurement to start from

A probe that sweeps every byte offset trying `Option<SceneGraph>` finds **no offset that parses
with a non-zero node count**. So the drift is inside the graph's own encoding, not in
`ProjectFile`'s prefix or tail. A `ProjectFile`-level legacy-shape fallback was written and
**did not help**, consistent with that.

Next step should be a byte-level walk of the graph encoding (node count, then each node's payload
tag and body) against the old shape, rather than more field-list diffing — the field lists have
now all been checked.

## Recommendation

Two separate things are true and worth separating:

1. **`SdfPrimitive`'s mid-struct insertion is a real save-format bug** regardless of this test.
   Worth fixing on its own merits — but note the nested-struct finding above: moving the fields to
   the end is *not* sufficient to make old files load, so the fix needs a real migration
   (versioned shape or an explicit legacy reader), not a reorder.
2. **The format has no working versioning story.** Fields were added in-blob under a false
   assumption about `#[serde(default)]`, and `version` stayed 7 throughout, so old and new files
   are indistinguishable. Until that is addressed, any further in-blob field is another instance
   of this class. This is the architectural fix; the rest are instances.
