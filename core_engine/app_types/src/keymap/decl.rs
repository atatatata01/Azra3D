// app_types/src/keymap/decl.rs — the declaration shape (INPUT_ARCHITECTURE_PLAN §3.3) + the S2
// legacy prose→When mapping.
//
// S2: `ActionDef.context: Option<&str>` becomes `guard: Guard` (+ hint/avail). Every action lands as
// `Guard::Legacy(when)` — ADVISORY: it feeds `find_conflicts` + the editor, but `resolve()` (S4)
// ignores Legacy, so behavior is unchanged. `rg 'Guard::Declared' | wc -l` is the migration progress
// bar; each action flips to `Declared` when its tool/pillar is migrated and the When re-verified
// against the actual dispatch guard.

use super::ctx::*;

/// Context declaration on an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guard {
    /// Context enforced by hand-written ifs at the dispatch site. `When` is ADVISORY (find_conflicts
    /// + editor only); `resolve()` skips it. THE MIGRATION LEDGER.
    Legacy(When),
    /// Context enforced by `resolve()`. `When` is authoritative and cannot drift from dispatch.
    Declared(When),
}
impl Guard {
    pub fn when(&self) -> When { match self { Guard::Legacy(w) | Guard::Declared(w) => *w } }
    pub fn is_declared(&self) -> bool { matches!(self, Guard::Declared(_)) }
}

/// Generated-hint-row participation (drives the hint row from S10). Inert until then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint { None, Row(u8) }

/// Narrow availability escape hatch (§3.5). `Poll` carries an id resolved by one capped function
/// (S4+). Inert until then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Avail { Always, Poll(u16) }

// ── The 16 distinct legacy prose contexts → When. Coarse by design (Legacy = advisory); each is
//    re-verified against the real dispatch guard when its action migrates to Declared (plan R3). ──
pub const WHEN_POLY_EDIT: When = When::poly_edit(ElemMask::ANY);                                   // "Polygonal edit mode"
pub const WHEN_POLY_VERT: When = When::poly_edit(ElemMask::only(ElemMode::Vertex));                // "Poly Vertex mode"
pub const WHEN_POLY_EDGE: When = When::poly_edit(ElemMask::only(ElemMode::Edge));                  // "Poly Edge mode"
pub const WHEN_POLY_FACE: When = When::poly_edit(ElemMask::only(ElemMode::Face));                  // "Poly Face mode"
pub const WHEN_POLY_VE:   When = When::poly_edit(ElemMask::only(ElemMode::Vertex).or(ElemMode::Edge)); // "Poly Vertex/Edge mode"
pub const WHEN_POLY_EF:   When = When::poly_edit(ElemMask::only(ElemMode::Edge).or(ElemMode::Face));   // "Poly Edge/Face mode"
pub const WHEN_POLYGONAL: When = When::only_pillar(Pillar::Polygonal);                             // "Polygonal mode"
pub const WHEN_OBJECT:    When = When::only_work(WorkKind::Object);                                // "Object mode"
pub const WHEN_SDF:       When = When::only_pillar(Pillar::Sdf);                                   // "SDF context" / "SDF edit / Body select"
pub const WHEN_NURBS:     When = When::only_pillar(Pillar::Nurbs);                                 // "NURBS mode"
pub const WHEN_NURBS_CV:  When = When::pillar_edit(Pillar::Nurbs);                                 // "NURBS CV mode"
pub const WHEN_CURVE:     When = When::only_pillar(Pillar::Curve);                                 // "Curve point editor"
pub const WHEN_CREATE:    When = When::only_tool(ToolId::Create);                                  // "During creation" / "Footprint phase"
/// "Active tool" — a confirm/cancel/axis key that fires while SOME tool is live. There is no single
/// mask for "any non-Idle tool"; kept ANY (context-blind) until each such key migrates to its tool.
pub const WHEN_ACTIVE_TOOL: When = When::ANY;
