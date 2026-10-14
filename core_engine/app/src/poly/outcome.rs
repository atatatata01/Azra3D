//! The outcome of a poly Euler batch + THE one shared reporter for it.
//!
//! WHY THIS FILE EXISTS: `poly_op_batch` used to return a bare `bool` (true = ">=1 op applied"), and
//! every `OpError` was dropped on the floor at the `if let Ok(inv)` in undo.rs. A caller therefore
//! could not tell "all 5 dissolved" from "1 of 5 dissolved" — so all 13 sites toasted unconditional
//! success and cleared the WHOLE selection, discarding the selection of elements that were never
//! touched. The user saw a confident "Edges dissolved" over a mesh that did not change, with no way
//! to see or retry the refused edges.
//!
//! The fix is structural, not a message tweak:
//!  - `PolyOpOutcome` CARRIES the refused ops (not a count), so the exact refused element ids are
//!    recoverable via `PolyEdit::targets()`.
//!  - `#[must_use]` makes ignoring the outcome a compile warning (extrude.rs:37 silently dropped the
//!    bool — that silent drop IS the bug class).
//!  - `poly_report_batch()` does BOTH halves — message AND selection retention — so no caller can do
//!    one and forget the other. Per CLAUDE.md "two code paths for the same operation MUST share one
//!    path": there is exactly ONE message builder, never a copy per call site.

use core_math::mesh_ops::{ElemDomain, OpError, PolyEdit};

use crate::AzraApp;

/// What a poly Euler batch actually did. Replaces the bare bool.
///
/// COUNTS ARE IN ELEMENTS, NOT OPS. One op does not mean one element: `DissolveFaces { faces }` is a
/// single region op covering N faces (dissolve.rs:33), and `MergeVerticesAt { verts }` covers a whole
/// cluster (merge.rs:99). Counting ops would report "1 face dissolved" after dissolving five, which
/// is the same class of lie this whole change exists to kill. The user selected ELEMENTS, so the
/// report speaks in elements — as Blender does ('Removed %d vertices').
#[must_use = "a poly batch outcome carries refusals — report it via poly_report_batch()"]
pub(crate) struct PolyOpOutcome {
    /// Number of OPS that applied and are recorded in the ONE `UndoPayload::Poly` entry. Undo/redo
    /// semantics only — never user-facing text (see the type doc).
    pub applied_ops: usize,
    /// Number of ELEMENTS the applied ops targeted. Computed in `poly_op_batch` before the applied
    /// ops move into the undo entry. This is the number the user is shown.
    pub applied_elems: usize,
    /// Every op that REFUSED, MOVED out of the submitted Vec (no clone) with its reason.
    /// Carrying the OP — not just a count — is what lets a caller retain the exact elements.
    pub refused: Vec<(PolyEdit, OpError)>,
    /// Domain of the SUBMITTED ops, so it is known even when nothing refused. `None` = a whole-mesh
    /// op (Solidify / SubdivideCatmullClark) that owns no selection domain.
    pub domain: Option<ElemDomain>,
    /// `false` = the batch never reached a mesh (node missing / not a PolygonalBody). Distinct from
    /// "ran and everything refused": that must report + retain; this must touch nothing.
    pub ran: bool,
}

impl PolyOpOutcome {
    pub fn not_run() -> Self {
        Self { applied_ops: 0, applied_elems: 0, refused: Vec::new(), domain: None, ran: false }
    }
    /// Drop-in for the old bool: `true` if >=1 op applied (an undo entry was pushed).
    pub fn any_applied(&self) -> bool { self.applied_ops > 0 }
    /// How many ELEMENTS the caller submitted across the whole batch.
    pub fn submitted_elems(&self) -> usize { self.applied_elems + self.refused_targets().len() }
    /// Exactly the elements the refused ops targeted — the set that must stay selected. Ids are
    /// still valid because a refusal leaves the mesh byte-identical (apply_in_place is atomic on
    /// Err, mesh_ops/mod.rs:307), so nothing renumbered under them.
    pub fn refused_targets(&self) -> Vec<u32> {
        self.refused.iter()
            .filter_map(|(op, _)| op.targets())
            .flat_map(|(_, ids)| ids)
            .collect()
    }
}

impl AzraApp {
    /// THE single reporter for every poly batch. Does BOTH halves so no caller can do one and forget
    /// the other: (1) toast the honest outcome, (2) retain the refused elements' selection.
    ///
    /// Selection contract — the reporter runs LAST and is authoritative for `out.domain`: it sets
    /// that one domain's set to `keep` ∪ refused-targets. Callers do any cross-domain work (e.g.
    /// subdivide's clear-all-three) BEFORE calling. `keep` = the caller's success-side selection
    /// (merge survivors, extruded/flipped faces); empty for delete/dissolve, where success
    /// deselects. A `domain: None` batch never touches selection.
    ///
    /// `past_verb` = "dissolved" / "deleted" / "merged" — lowercase, used mid-sentence.
    ///
    /// Wording follows the Blender oracle's harvested report templates: always carry the count
    /// (`'Removed %d vertices'` prints even at 0), use its N-of-M form for partials
    /// (`'Skipped %d of %d island(s), <reason>'`), and respect the singular (`'Removed %d vertex'`).
    /// Blender is SILENT on a full refusal; we deliberately are not — see the module doc: the whole
    /// point is that a refusal must say WHY. `\u{274C}` is the existing severity convention
    /// (app_poll_ai.rs:18/:68/:84) — `Toast` has no severity field, so do not invent a second one.
    pub(crate) fn poly_report_batch(&mut self, out: &PolyOpOutcome, past_verb: &str, keep: &[u32]) {
        if !out.ran { return; } // never reached a mesh → say nothing, change nothing.

        // (1) SELECTION — retain exactly what refused, plus whatever the caller kept.
        if let Some(d) = out.domain {
            let refused = out.refused_targets();
            let set = match d {
                ElemDomain::Vertex   => &mut self.poly_sel.selected_vertices,
                ElemDomain::HalfEdge => &mut self.poly_sel.selected_edges,
                ElemDomain::Face     => &mut self.poly_sel.selected_faces,
            };
            set.clear();
            set.extend(keep.iter().copied());
            set.extend(refused); // <- THE RETENTION: refused elements stay visible + retryable.
        }

        // (2) MESSAGE — one builder, never copied to a call site.
        let msg = self.poly_batch_message(out, past_verb);
        self.toast(&msg);
    }

    /// The ONE message builder. Split out only so the reporter's two halves each do one thing.
    fn poly_batch_message(&self, out: &PolyOpOutcome, past_verb: &str) -> String {
        // Whole-mesh ops (Solidify, SubdivideCatmullClark) target no elements, so an element count
        // would always read "0 elements solidified". They get count-free phrasing.
        let Some(domain) = out.domain else {
            return match out.refused.first() {
                None => format!("Mesh {}", past_verb),
                Some((_, e)) => format!("\u{274C} Nothing {} — {}", past_verb, e),
            };
        };
        let noun = |n: usize| -> &'static str {
            match domain {
                ElemDomain::Vertex   => if n == 1 { "vertex" } else { "vertices" },
                ElemDomain::HalfEdge => if n == 1 { "edge" } else { "edges" },
                ElemDomain::Face     => if n == 1 { "face" } else { "faces" },
            }
        };
        match (out.applied_elems, out.refused.first()) {
            // Everything applied. Blender always prints the count ('Removed %d vertices').
            (n, None) => format!("{} {} {}", n, noun(n), past_verb),
            // Nothing applied. Never a success claim — say why (OpError: Display, mesh_ops/mod.rs:62).
            (0, Some((_, e))) => format!("\u{274C} Nothing {} — {}", past_verb, e),
            // PARTIAL — the case the old bare bool could not express at all.
            // Blender template: 'Skipped %d of %d island(s), <reason>'.
            (n, Some((_, e))) => {
                let total = out.submitted_elems();
                format!("{} of {} {} {} — {} refused ({}); refused stay selected",
                        n, total, noun(total), past_verb, out.refused_targets().len(), e)
            }
        }
    }
}
