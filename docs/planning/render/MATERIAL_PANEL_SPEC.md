# Material Panel Spec — ownership & authoring surface

**Date:** 2026-07-31 · **Branch:** `feat/material-render`  
**Related:** `PLAN_FOR_GROK_MATERIALS.md` (owner decisions D1–D4), `MATERIAL_RENDER_ARCHITECTURE.md`
(rules M1–M3), `MATERIAL_COMPOSITE_ADR.md` (bake = compositor; per-parameter ownership),
`docs/panel_node_mapping.md` (widget → node table).

This file anchors **panel ownership**. Historical phase notes (P0–P8) live in commit messages and
implementation under `core_engine/app/src/material_panel/`; this document records settled product
rules so later milestones do not re-litigate them.

---

## D1 — The layer PANEL is the authoring surface (settled 2026-07-31)

**The layer panel is the canonical authoring surface for materials.** Panel-created materials are
the source of truth: the node graph is generated under the hood from the panel document
(`MaterialPanelDoc` → `regen` / merge lowering). The node editor remains available for power users
and for advanced graph surgery on panel-made materials, but **no feature may require opening the
node graph to be usable**, and **no milestone may build two-way panel↔graph sync as a
prerequisite**. Panel → graph is one-way authoritative; graph edits to panel-made materials are
advanced-mode (preserve-when-possible under merge regen / per-parameter `Const|Driven`), not the
sync target. This resolves the parity-plan §7 ADR in favor of panel-first UX and is the binding
interpretation of COMPOSITE_ADR Decision 2 for all work on this branch.

---

## Pointers (do not duplicate)

| Concern | Authority |
|---|---|
| Who composites layers | Bake only — `MATERIAL_RENDER_ARCHITECTURE.md` rule M1; G-buffer is display |
| CPU vs GPU bake | CPU oracle, GPU product — rule M2 |
| Packed map bits | One `TexMap` word — rule M3 |
| Widget → node mapping | `docs/panel_node_mapping.md` |
| Milestone order | `PLAN_FOR_GROK_MATERIALS.md` M0–M6 |
