# Lab — Cross-Cutting Experimental Features (Index)

The **lab** worktree (`feat/lab`) is for CRAZY, cross-cutting features that touch **more than one
subsystem** (e.g. sculpt + poly + render) and are **iterative / experimental** — you try things, most
fail, some stick.

**Discipline (non-negotiable):** every feature gets its OWN log file here. Log EVERYTHING you try —
especially the dead-ends — so we never re-derive a failure. This is the same knowledge-base discipline
the project already uses for `docs/planning/poly/BEVEL.md` (its §6 graveyard of ~22 failed approaches is
why the bevel work never loops back onto a known dead-end).

## Features
| Feature | Status | Touches | Log |
|---------|--------|---------|-----|
| Poly mesh → SDF (`MeshVolume` prim: destructive convert + LIVE linked modifier) | PROMISING (i3: link/follow/refresh built, parity exact; needs in-app smoke) | poly + SDF bake + graphics + io_bridge + core_math + scene graph | [poly-mesh-sdf-operand.md](poly-mesh-sdf-operand.md) |

## How to run the lab
- **New feature:** copy `_TEMPLATE.md` → `docs/planning/lab/<feature-kebab>.md`, add a row above.
- **Every iteration/session:** update that feature's log — new attempt + its outcome, any new dead-end,
  and the current §"Next step". A feature is not "figured out in your head" until its log says so.
- **Status values:** EXPLORING · STALLED · PROMISING · READY-TO-MERGE · PARKED · SHIPPED.

## Integration
- Commit to `feat/lab`. When a feature is READY-TO-MERGE (green + smoke-tested + its log current), tell
  the MASTER window — it's the single integrator + code-health gate and will merge it into master.
- Because features here span subsystems, call out in the log WHICH files/pillars they touch so master
  can watch for collisions with the poly / sculpt / cache / viewport windows at integration time.
