//! Outliner drop-slot semantics — PURE decision layer for drag-and-drop.
//!
//! Zero egui. The egui layer (outliner_draw_body.rs / outliner_dnd.rs) only measures rects,
//! classifies the pointer into a `HeaderZone`, and maps the returned semantics onto the
//! transient drop state + indicator drawing. The mutations live in scene_actions.rs /
//! prim_actions.rs (`reparent_body_node`, `move_prims_to_body_slot`). Keeping this layer
//! pure makes the whole slot matrix unit-testable headlessly (tests at the bottom).

use core_math::scene_graph::{BodyChildRef, NodeId};

/// Vertical zone of a row rect during a drag (top/bottom thirds vs center).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeaderZone {
    Top,
    Center,
    Bottom,
}

/// What a PRIM drag hovering an SdfBody header resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrimOverBodyHeader {
    /// Insert into `body`'s `child_order` at `index` (a between-slot around a sub-body row).
    InsertIntoOrder { body: NodeId, index: usize },
    /// Reparent the dragged prim(s) INTO this body (append).
    ReparentInto { body: NodeId },
}

/// Resolve a prim drag over a body header row.
///
/// `parent_pos`: `Some((parent_body, position_of_this_body_in_parent_child_order))` when the
/// hovered header is a SUB-body of an SdfBody; `None` for top-level bodies (or when the ref
/// is missing from the parent's order — self-heal case). Top-level headers offer only
/// "reparent into" over the whole rect: the space BETWEEN top-level rows belongs to the
/// root/new-body path, so between-slots exist only inside a body's own stack.
pub(crate) fn resolve_prim_over_body_header(
    header_body: NodeId,
    parent_pos: Option<(NodeId, usize)>,
    zone: HeaderZone,
) -> PrimOverBodyHeader {
    match (parent_pos, zone) {
        (Some((parent, pos)), HeaderZone::Top) => {
            PrimOverBodyHeader::InsertIntoOrder { body: parent, index: pos }
        }
        (Some((parent, pos)), HeaderZone::Bottom) => {
            // AFTER the group = after its WHOLE subtree: the group is one ref in the
            // parent's child_order, so +1 lands past everything it contains.
            PrimOverBodyHeader::InsertIntoOrder { body: parent, index: pos + 1 }
        }
        _ => PrimOverBodyHeader::ReparentInto { body: header_body },
    }
}

/// What a prim drag hovering a PRIM row resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrimOverPrimRow {
    /// Insert into the row's body `child_order` at `index` — the canonical slot the release
    /// applies via `move_prims_to_body_slot` (same-body reorder AND cross-body move share it).
    Insert { body: NodeId, index: usize },
    /// Center zone: group the dragged prim(s) with the row's prim.
    Group,
}

/// Resolve a prim drag over another PRIM's row into the canonical `child_order` slot.
///
/// This is the SAME (body, index) representation used by the sub-body slots and the
/// end-of-body strip — never a flat prim index. Flat `flat_idx ± 1` slots were the root
/// cause of the dead/append-degraded between-slot drops: `flat_idx + 1` on the last row is
/// out of range, and on a body's last row it silently crosses into the NEXT body's rows.
/// `row_uid` = the hovered row's prim uid; `order` = its body's reconciled child_order.
pub(crate) fn resolve_prim_over_prim_row(
    order: &[BodyChildRef],
    body: NodeId,
    row_uid: u32,
    zone: HeaderZone,
) -> PrimOverPrimRow {
    // Missing ref (stale order mid-mutation) self-heals to the end slot.
    let pos = order
        .iter()
        .position(|&r| r == BodyChildRef::Prim(row_uid))
        .unwrap_or(order.len());
    match zone {
        HeaderZone::Top => PrimOverPrimRow::Insert { body, index: pos },
        HeaderZone::Bottom => PrimOverPrimRow::Insert { body, index: pos + 1 },
        HeaderZone::Center => PrimOverPrimRow::Group,
    }
}

/// What a BODY drag hovering another body header resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BodyOverBodyHeader {
    /// Reorder / move the dragged body to sit BEFORE (`before = true`) or AFTER the target
    /// row, under the TARGET's parent (cross-parent drops reparent first — un-nest between
    /// top-level rows, re-nest between a group's children).
    PlaceNextTo { before: bool },
    /// Nest the dragged body INTO the target (append at the end of its child_order).
    NestInto,
    /// No legal slot (self-drop, or the target sits inside the dragged body's own subtree —
    /// the CYCLE GUARD, enforced here at slot-enumeration level so no indicator is offered).
    Rejected,
}

/// Resolve a body drag over another body header. `dragged` = the FULL dragged set (a
/// multi-body drag carries every selected top-most body). `target_inside_dragged` = the
/// hovered header is one of the dragged bodies or a descendant of ANY of them (cycle) —
/// if ANY dragged body is an ancestor of the target, the whole drop is Rejected: no slot,
/// no indicator, no release action.
pub(crate) fn resolve_body_over_body_header(
    dragged: &[NodeId],
    target: NodeId,
    target_inside_dragged: bool,
    zone: HeaderZone,
) -> BodyOverBodyHeader {
    if dragged.contains(&target) || target_inside_dragged {
        return BodyOverBodyHeader::Rejected;
    }
    match zone {
        HeaderZone::Top => BodyOverBodyHeader::PlaceNextTo { before: true },
        HeaderZone::Bottom => BodyOverBodyHeader::PlaceNextTo { before: false },
        HeaderZone::Center => BodyOverBodyHeader::NestInto,
    }
}

/// Adjust a PRE-removal insert index for a remove-then-insert reorder: when the dragged ref
/// currently sits BEFORE the raw insert point, the removal shifts the point down by one.
/// (`from_pos` = current position of the dragged ref, `None` when it isn't in the list yet.)
pub(crate) fn insert_index_after_removal(from_pos: Option<usize>, raw: usize) -> usize {
    match from_pos {
        Some(fp) if fp < raw => raw - 1,
        _ => raw,
    }
}

/// The ANCHOR for a slot insert: the first ref at/after `insert_index` that is NOT itself
/// being moved. `None` = insert at the end. Anchoring (instead of a raw index) keeps a
/// multi-prim block landing contiguously and in payload order even while earlier moves
/// shift the list under us.
/// Generic over the ref type so the SAME slot math serves both a body's `child_order`
/// (`BodyChildRef`) and a plain node-edge children list (`NodeId` — top-level body reorder).
pub(crate) fn slot_anchor<T: Copy + PartialEq>(
    order: &[T],
    insert_index: usize,
    moved: &[T],
) -> Option<T> {
    order
        .get(insert_index.min(order.len())..)
        .and_then(|tail| tail.iter().find(|r| !moved.contains(r)).copied())
}

/// Where to insert `dragged` in `order` so it lands immediately BEFORE `anchor`
/// (or at the end when `anchor` is `None` / missing), given that the reorder op
/// removes `dragged` first.
pub(crate) fn slot_insert_position<T: Copy + PartialEq>(
    order: &[T],
    dragged: T,
    anchor: Option<T>,
) -> usize {
    let from_pos = order.iter().position(|&r| r == dragged);
    match anchor.and_then(|a| order.iter().position(|&r| r == a)) {
        Some(ap) => insert_index_after_removal(from_pos, ap),
        None => order.len().saturating_sub(from_pos.is_some() as usize),
    }
}

/// True when dropping a SINGLE prim at `insert_index` of its OWN body would change nothing
/// (the slot is directly before or after the prim's current position).
pub(crate) fn slot_is_noop(order: &[BodyChildRef], dragged: BodyChildRef, insert_index: usize) -> bool {
    order.get(insert_index) == Some(&dragged)
        || (insert_index > 0 && order.get(insert_index - 1) == Some(&dragged))
}

// =====================================================================
// Indicator geometry + zone partition + labels (pure, egui-free).
// =====================================================================

/// The vertical BAND a row owns for drop hit-testing: its rect extended by half the
/// uniform item spacing on each side, half-open `[top - s/2, bottom + s/2)`. Adjacent
/// rows' bands tile the tree area EXACTLY (gaps split at the midpoint) — every vertical
/// pixel while dragging maps to exactly one row, no dead pixels, no overlaps.
pub(crate) fn row_band(top: f32, bottom: f32, spacing: f32) -> (f32, f32) {
    (top - spacing * 0.5, bottom + spacing * 0.5)
}

/// Half-open band membership (see `row_band` — half-open keeps adjacent bands disjoint).
pub(crate) fn band_contains(band: (f32, f32), y: f32) -> bool {
    y >= band.0 && y < band.1
}

/// Classify a cursor y against a row rect into Top / Center / Bottom. `edge_frac` is the
/// fraction of the row height each edge zone takes (prim rows + prim-over-header use 1/3;
/// body-over-header uses 0.30 to widen the nest zone). `y` may lie OUTSIDE the rect (the
/// row's band extends into the spacing gaps): above maps to Top, below to Bottom, so the
/// gap pixels resolve to the nearest between-slot.
pub(crate) fn classify_zone(top: f32, bottom: f32, y: f32, edge_frac: f32) -> HeaderZone {
    let h = bottom - top;
    if y < top + h * edge_frac {
        HeaderZone::Top
    } else if y > bottom - h * edge_frac {
        HeaderZone::Bottom
    } else {
        HeaderZone::Center
    }
}

/// Blender's X-RESOLUTION rule for the ambiguous end-of-container position: when one
/// cursor y offers slots at several depths (end of the innermost group, after that group
/// in its parent, …, root/new-body), the cursor X picks the depth. `candidates` are
/// ordered DEEPEST-FIRST with strictly descending indent x. Returns the deepest candidate
/// whose indent lies at/left of the cursor; a cursor left of every child indent falls to
/// the SHALLOWEST candidate.
pub(crate) fn pick_by_indent<T>(candidates: &[(f32, T)], cursor_x: f32) -> Option<&T> {
    candidates
        .iter()
        .find(|(x, _)| cursor_x >= *x)
        .or_else(|| candidates.last())
        .map(|(_, t)| t)
}

/// Human-readable action a resolved drop slot will perform on release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlotAction<'a> {
    /// Insert before the named row (between-slot).
    Before(&'a str),
    /// Insert after the named row / subtree (between-slot).
    After(&'a str),
    /// Reparent into the named container (drop-INTO).
    Into(&'a str),
    /// Append at the end of the named body's stack (end-of-body strip).
    EndOf(&'a str),
    /// Extract into a brand-new top-level body (root zone, prim drag).
    NewBody,
    /// Un-nest to the graph root (root zone, body drag).
    ToRoot,
    /// Group with the named prim (drop ON a prim).
    Group(&'a str),
}

/// The ghost/indicator label for a slot: the exact verb the release will perform,
/// switching Move → Copy when Ctrl is held.
pub(crate) fn slot_label(action: SlotAction, copy: bool) -> String {
    let verb = if copy { "Copy" } else { "Move" };
    match action {
        SlotAction::Before(n) => format!("{verb} before {n}"),
        SlotAction::After(n) => format!("{verb} after {n}"),
        SlotAction::Into(n) => format!("{verb} into {n}"),
        SlotAction::EndOf(n) => format!("{verb} to end of {n}"),
        SlotAction::NewBody => {
            if copy { "Copy \u{2192} new body".into() } else { "New body".into() }
        }
        SlotAction::ToRoot => format!("{verb} to top level"),
        SlotAction::Group(n) => format!("Group with {n}"),
    }
}

// =====================================================================
// Tests — the drop-slot matrix, headless.
// =====================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use BodyChildRef::{Body, Prim};

    fn nid(i: u64) -> NodeId { NodeId(i) }

    // ---- prim over body header ----

    #[test]
    fn prim_over_subbody_top_inserts_before_group() {
        // Body.001 order: [Prim(1)=Box.001, Body(10)=Group]; hovering Group's top third.
        let r = resolve_prim_over_body_header(nid(10), Some((nid(1), 1)), HeaderZone::Top);
        assert_eq!(r, PrimOverBodyHeader::InsertIntoOrder { body: nid(1), index: 1 });
    }

    #[test]
    fn prim_over_subbody_bottom_inserts_after_group_subtree() {
        // The AFTER-group slot (owner bug 1): one past the group's single ref.
        let r = resolve_prim_over_body_header(nid(10), Some((nid(1), 1)), HeaderZone::Bottom);
        assert_eq!(r, PrimOverBodyHeader::InsertIntoOrder { body: nid(1), index: 2 });
    }

    #[test]
    fn prim_over_subbody_center_reparents_into_group() {
        let r = resolve_prim_over_body_header(nid(10), Some((nid(1), 1)), HeaderZone::Center);
        assert_eq!(r, PrimOverBodyHeader::ReparentInto { body: nid(10) });
    }

    #[test]
    fn prim_over_toplevel_body_always_reparents_into() {
        for z in [HeaderZone::Top, HeaderZone::Center, HeaderZone::Bottom] {
            let r = resolve_prim_over_body_header(nid(2), None, z);
            assert_eq!(r, PrimOverBodyHeader::ReparentInto { body: nid(2) });
        }
    }

    // ---- body over body header ----

    #[test]
    fn body_zones_map_to_place_and_nest() {
        assert_eq!(resolve_body_over_body_header(&[nid(1)], nid(2), false, HeaderZone::Top),
            BodyOverBodyHeader::PlaceNextTo { before: true });
        assert_eq!(resolve_body_over_body_header(&[nid(1)], nid(2), false, HeaderZone::Bottom),
            BodyOverBodyHeader::PlaceNextTo { before: false });
        assert_eq!(resolve_body_over_body_header(&[nid(1)], nid(2), false, HeaderZone::Center),
            BodyOverBodyHeader::NestInto);
    }

    #[test]
    fn body_cycle_guard_rejects_at_enumeration_level() {
        // Dropping a body onto itself or its own descendant offers NO slot at all.
        for z in [HeaderZone::Top, HeaderZone::Center, HeaderZone::Bottom] {
            assert_eq!(resolve_body_over_body_header(&[nid(1)], nid(1), false, z), BodyOverBodyHeader::Rejected);
            assert_eq!(resolve_body_over_body_header(&[nid(1)], nid(5), true, z), BodyOverBodyHeader::Rejected);
        }
    }

    #[test]
    fn multi_body_cycle_guard_rejects_if_any_dragged_is_target_or_ancestor() {
        // Multi-body drag: target == ANY member of the dragged set → Rejected.
        for z in [HeaderZone::Top, HeaderZone::Center, HeaderZone::Bottom] {
            assert_eq!(resolve_body_over_body_header(&[nid(1), nid(2)], nid(2), false, z),
                BodyOverBodyHeader::Rejected);
            // target inside ANY dragged body's subtree (caller ORs the descendant checks).
            assert_eq!(resolve_body_over_body_header(&[nid(1), nid(2)], nid(9), true, z),
                BodyOverBodyHeader::Rejected);
        }
        // A clean multi-set still resolves normally.
        assert_eq!(resolve_body_over_body_header(&[nid(1), nid(2)], nid(3), false, HeaderZone::Center),
            BodyOverBodyHeader::NestInto);
    }

    #[test]
    fn multi_body_block_lands_contiguous_next_to_target() {
        // Node-edge (top-level) reorder path, generic slot math over NodeId:
        // children [A,B,C,D], drag [B, D] to sit BEFORE C ⇒ [A, B, D, C].
        let mut children = vec![nid(1), nid(2), nid(3), nid(4)];
        let moved = [nid(2), nid(4)];
        let raw = children.iter().position(|&c| c == nid(3)).unwrap(); // before C
        let anchor = slot_anchor(&children, raw, &moved);
        assert_eq!(anchor, Some(nid(3)));
        for &m in &moved {
            let pos = slot_insert_position(&children, m, anchor);
            children.retain(|&c| c != m);
            children.insert(pos.min(children.len()), m);
        }
        assert_eq!(children, vec![nid(1), nid(2), nid(4), nid(3)]);
    }

    // ---- slot math (child_order = the boolean chain order) ----

    #[test]
    fn insert_index_compensates_for_prior_removal() {
        assert_eq!(insert_index_after_removal(Some(0), 3), 2); // downward move
        assert_eq!(insert_index_after_removal(Some(3), 1), 1); // upward move
        assert_eq!(insert_index_after_removal(None, 2), 2);    // cross-body (not in list yet)
    }

    #[test]
    fn same_body_reorder_prim_after_group() {
        // Owner bug 1: [P1, Group, P3] — drop P1 into slot AFTER the group (index 2).
        let order = [Prim(1), Body(nid(10)), Prim(3)];
        let anchor = slot_anchor(&order, 2, &[Prim(1)]);
        assert_eq!(anchor, Some(Prim(3)));
        // remove P1 → [Group, P3]; insert before P3 → index 1 → [Group, P1, P3].
        assert_eq!(slot_insert_position(&order, Prim(1), anchor), 1);
    }

    #[test]
    fn cross_body_move_lands_at_slot() {
        // Owner bug 2: prim 20 dragged OUT of the sub-body into the parent's stack at slot 1
        // (between P1 and the Group). It is not yet in the parent's order.
        let order = [Prim(1), Body(nid(10))];
        let anchor = slot_anchor(&order, 1, &[Prim(20)]);
        assert_eq!(anchor, Some(Body(nid(10))));
        // After the reparent appends it: [P1, Group, P20] → move before Group → index 1.
        let post = [Prim(1), Body(nid(10)), Prim(20)];
        assert_eq!(slot_insert_position(&post, Prim(20), anchor), 1);
    }

    #[test]
    fn end_of_body_slot_appends() {
        let order = [Prim(1), Body(nid(10))];
        let anchor = slot_anchor(&order, order.len(), &[Prim(1)]);
        assert_eq!(anchor, None);
        // P1 is in the list → end after removal = 1 → [Group, P1].
        assert_eq!(slot_insert_position(&order, Prim(1), anchor), 1);
        // Cross-body ref (not in list) appends at raw end.
        assert_eq!(slot_insert_position(&order, Prim(99), None), 2);
    }

    #[test]
    fn multi_prim_block_lands_contiguous_in_payload_order() {
        // Simulate the apply loop: move [P2, P4] to slot 0 of [P1, P2, P3, P4].
        let mut order = vec![Prim(1), Prim(2), Prim(3), Prim(4)];
        let moved = [Prim(2), Prim(4)];
        let anchor = slot_anchor(&order, 0, &moved); // P1 (not moved)
        assert_eq!(anchor, Some(Prim(1)));
        for &m in &moved {
            let pos = slot_insert_position(&order, m, anchor);
            order.retain(|&r| r != m);
            order.insert(pos.min(order.len()), m);
        }
        assert_eq!(order, vec![Prim(2), Prim(4), Prim(1), Prim(3)]);
    }

    #[test]
    fn anchor_skips_refs_being_moved() {
        // Dropping [P2, P3] right before P2 itself: anchor must skip past the moved block.
        let order = [Prim(1), Prim(2), Prim(3), Prim(4)];
        let anchor = slot_anchor(&order, 1, &[Prim(2), Prim(3)]);
        assert_eq!(anchor, Some(Prim(4)));
    }

    #[test]
    fn single_prim_noop_slots_detected() {
        let order = [Prim(1), Body(nid(10)), Prim(3)];
        assert!(slot_is_noop(&order, Prim(1), 0)); // before itself
        assert!(slot_is_noop(&order, Prim(1), 1)); // right after itself
        assert!(!slot_is_noop(&order, Prim(1), 2)); // a real move
        assert!(!slot_is_noop(&order, Prim(3), 1));
    }

    // ---- zone partition: every drag-time y maps to exactly ONE slot ----

    #[test]
    fn zone_coverage_no_dead_pixels_no_overlaps() {
        // Synthetic tree: rows of mixed heights with uniform 2px item spacing
        // (prim rows 22px, a header 20px, an end-of-body strip 11px).
        let spacing = 2.0;
        let heights = [22.0, 22.0, 20.0, 22.0, 11.0, 22.0];
        let mut rows: Vec<(f32, f32)> = Vec::new();
        let mut y = 10.0;
        for h in heights {
            rows.push((y, y + h));
            y += h + spacing;
        }
        let bands: Vec<(f32, f32)> = rows.iter().map(|&(t, b)| row_band(t, b, spacing)).collect();
        // Sweep the whole tree area (band of first row start → band of last row end)
        // at sub-pixel steps: EVERY y hits exactly one band.
        let area = (bands.first().unwrap().0, bands.last().unwrap().1);
        let mut yy = area.0;
        while yy < area.1 {
            let hits = bands.iter().filter(|&&b| band_contains(b, yy)).count();
            assert_eq!(hits, 1, "y={yy} owned by {hits} slots (must be exactly 1)");
            yy += 0.25;
        }
        // Below the last band = the root/new-body zone (owned by the fallback, not a row).
        assert!(!bands.iter().any(|&b| band_contains(b, area.1 + 0.5)));
    }

    #[test]
    fn gap_pixels_resolve_to_nearest_between_slot() {
        // A y in the spacing gap BELOW a row is inside that row's band and classifies
        // as Bottom (= insert after); a y in the gap ABOVE classifies as Top.
        let (top, bottom, s) = (100.0, 122.0, 2.0);
        let band = row_band(top, bottom, s);
        assert!(band_contains(band, bottom + 0.9)); // gap below
        assert_eq!(classify_zone(top, bottom, bottom + 0.9, 1.0 / 3.0), HeaderZone::Bottom);
        assert!(band_contains(band, top - 0.9)); // gap above
        assert_eq!(classify_zone(top, bottom, top - 0.9, 1.0 / 3.0), HeaderZone::Top);
        // Interior thirds unchanged.
        assert_eq!(classify_zone(top, bottom, 111.0, 1.0 / 3.0), HeaderZone::Center);
    }

    // ---- Blender X-resolution at the ambiguous end-of-nested-list position ----

    #[test]
    fn x_resolution_last_child_two_depths() {
        // Body.001 > Group.001 (last child). At the strip below Group's last row two
        // slots coexist: end of Group (indent 40) vs after Group in Body.001 (indent 20).
        // Cursor right of the child indent → INTO the group; left → AFTER the group.
        let cands = [(40.0, "end of Group.001"), (20.0, "after Group.001")];
        assert_eq!(pick_by_indent(&cands, 55.0), Some(&"end of Group.001"));
        assert_eq!(pick_by_indent(&cands, 40.0), Some(&"end of Group.001")); // at the indent = in
        assert_eq!(pick_by_indent(&cands, 30.0), Some(&"after Group.001"));
        assert_eq!(pick_by_indent(&cands, 5.0), Some(&"after Group.001")); // far left → shallowest
    }

    #[test]
    fn x_resolution_last_child_three_depths() {
        // Body > Group > SubGroup, SubGroup last in Group, Group last in Body: THREE
        // depths + the root/new-body zone share one cursor y. Deepest whose indent ≤ x wins.
        let cands = [
            (60.0, Some("end of SubGroup")),
            (40.0, Some("after SubGroup (end of Group)")),
            (20.0, Some("after Group (end of Body)")),
            (0.0, None), // root → new body
        ];
        assert_eq!(pick_by_indent(&cands, 70.0), Some(&Some("end of SubGroup")));
        assert_eq!(pick_by_indent(&cands, 45.0), Some(&Some("after SubGroup (end of Group)")));
        assert_eq!(pick_by_indent(&cands, 25.0), Some(&Some("after Group (end of Body)")));
        // Between root indent and depth-1 indent → the ROOT zone (deepest whose indent ≤ x).
        assert_eq!(pick_by_indent(&cands, 10.0), Some(&None));
        assert_eq!(pick_by_indent(&cands, -3.0), Some(&None)); // left of everything → shallowest
        assert_eq!(pick_by_indent::<&str>(&[], 10.0), None);
    }

    // ---- labels ----

    #[test]
    fn slot_labels_read_as_the_action() {
        assert_eq!(slot_label(SlotAction::After("Box.003"), false), "Move after Box.003");
        assert_eq!(slot_label(SlotAction::Into("Group.001"), false), "Move into Group.001");
        assert_eq!(slot_label(SlotAction::Into("Body.002"), true), "Copy into Body.002");
        assert_eq!(slot_label(SlotAction::Before("Sphere"), true), "Copy before Sphere");
        assert_eq!(slot_label(SlotAction::EndOf("Body.001"), false), "Move to end of Body.001");
        assert_eq!(slot_label(SlotAction::NewBody, false), "New body");
        assert_eq!(slot_label(SlotAction::NewBody, true), "Copy \u{2192} new body");
        assert_eq!(slot_label(SlotAction::ToRoot, false), "Move to top level");
        assert_eq!(slot_label(SlotAction::Group("Box"), false), "Group with Box");
    }

    #[test]
    fn anchor_out_of_range_is_end() {
        let order = [Prim(1)];
        assert_eq!(slot_anchor(&order, 5, &[]), None);
    }

    #[test]
    fn last_prim_dropped_below_itself_is_clean_noop() {
        // Review class "flat_idx+1": drag the LAST prim of a body into the end-of-body
        // strip / below the last row. Must be a valid end slot that leaves the order
        // unchanged — never a fall-through to root/new-body or a panic.
        let order = vec![Prim(1), Body(nid(10)), Prim(2)];
        let insert_index = order.len(); // end-of-body slot
        // Detected as a no-op for the single-prim case...
        assert!(slot_is_noop(&order, Prim(2), insert_index));
        // ...and even if applied, the remove-then-insert lands it back in place.
        let anchor = slot_anchor(&order, insert_index, &[Prim(2)]);
        assert_eq!(anchor, None);
        let pos = slot_insert_position(&order, Prim(2), anchor);
        let mut applied = order.clone();
        applied.retain(|&r| r != Prim(2));
        applied.insert(pos.min(applied.len()), Prim(2));
        assert_eq!(applied, order);
    }

    // ---- prim over PRIM row → canonical child_order slot (owner bug: dead / append drops) ----

    /// Simulate the release apply exactly as `move_prims_to_body_slot` does for the
    /// SAME-body case: anchor once, then remove-and-insert each moved ref.
    fn apply_same_body(order: &[BodyChildRef], moved: &[BodyChildRef], insert_index: usize) -> Vec<BodyChildRef> {
        let anchor = slot_anchor(order, insert_index, moved);
        let mut out = order.to_vec();
        for &m in moved {
            let pos = slot_insert_position(&out, m, anchor);
            out.retain(|&r| r != m);
            out.insert(pos.min(out.len()), m);
        }
        out
    }

    /// Cross-body: `reparent_prim_to_body` APPENDS the ref to the target order (prims-first
    /// reconcile), then `reorder_child_in_body` places it at the slot — mirror both steps.
    fn apply_cross_body(target_order: &[BodyChildRef], moved: &[BodyChildRef], insert_index: usize) -> Vec<BodyChildRef> {
        let anchor = slot_anchor(target_order, insert_index, moved);
        let mut out = target_order.to_vec();
        for &m in moved {
            out.push(m); // reparent appends
            let pos = slot_insert_position(&out, m, anchor);
            out.retain(|&r| r != m);
            out.insert(pos.min(out.len()), m);
        }
        out
    }

    #[test]
    fn prim_row_zones_resolve_to_child_order_slots() {
        // Body order [Cyl7, B4, B6]: Top of B4's row = index 1, Bottom = 2, Bottom of
        // LAST row (B6) = 3 (== len, the end slot the flat `flat_idx+1` could not express).
        let order = [Prim(7), Prim(4), Prim(6)];
        let b = nid(1);
        assert_eq!(resolve_prim_over_prim_row(&order, b, 4, HeaderZone::Top),
            PrimOverPrimRow::Insert { body: b, index: 1 });
        assert_eq!(resolve_prim_over_prim_row(&order, b, 4, HeaderZone::Bottom),
            PrimOverPrimRow::Insert { body: b, index: 2 });
        assert_eq!(resolve_prim_over_prim_row(&order, b, 6, HeaderZone::Bottom),
            PrimOverPrimRow::Insert { body: b, index: 3 });
        assert_eq!(resolve_prim_over_prim_row(&order, b, 6, HeaderZone::Center),
            PrimOverPrimRow::Group);
        // Stale row uid self-heals to the end slot (never a panic / dead slot).
        assert_eq!(resolve_prim_over_prim_row(&order, b, 99, HeaderZone::Top),
            PrimOverPrimRow::Insert { body: b, index: 3 });
    }

    #[test]
    fn owner_case_first_prim_to_end_slot_applies() {
        // Owner screenshot 1: [Cylinder.007, Box.004, Box.006], drag Cyl.007 to
        // "after Box.006" (end slot). Must yield [B4, B6, Cyl7] — not a dead release.
        let order = [Prim(7), Prim(4), Prim(6)];
        let slot = resolve_prim_over_prim_row(&order, nid(1), 6, HeaderZone::Bottom);
        let PrimOverPrimRow::Insert { index, .. } = slot else { panic!("expected slot") };
        // NOT a no-op for the first/middle prim (the guard only trips for adjacent slots).
        assert!(!slot_is_noop(&order, Prim(7), index));
        assert_eq!(apply_same_body(&order, &[Prim(7)], index), vec![Prim(4), Prim(6), Prim(7)]);
        // Middle prim to end.
        assert!(!slot_is_noop(&order, Prim(4), index));
        assert_eq!(apply_same_body(&order, &[Prim(4)], index), vec![Prim(7), Prim(6), Prim(4)]);
        // Last prim to end = clean no-op (batch-1 pin, still holds).
        assert!(slot_is_noop(&order, Prim(6), index));
        assert_eq!(apply_same_body(&order, &[Prim(6)], index), order.to_vec());
    }

    #[test]
    fn owner_case_cross_body_before_first_lands_first_not_appended() {
        // Owner screenshot 3: drag Box.003 from body B onto "before Box.004" in body A
        // (order [B4, B6, Cyl7]). Must land FIRST: [B3, B4, B6, Cyl7] — the old path
        // appended it to the end.
        let target = [Prim(4), Prim(6), Prim(7)];
        let slot = resolve_prim_over_prim_row(&target, nid(1), 4, HeaderZone::Top);
        let PrimOverPrimRow::Insert { index, .. } = slot else { panic!("expected slot") };
        assert_eq!(index, 0);
        assert_eq!(apply_cross_body(&target, &[Prim(3)], index),
            vec![Prim(3), Prim(4), Prim(6), Prim(7)]);
    }

    #[test]
    fn owner_case_multi_cross_body_after_last_applies_in_payload_order() {
        // Owner screenshot 4: drag {Cyl.007, Box.003} from body B onto "after Box.001"
        // (last row) in body A [B1]. Old path: flat_idx+1 crossed into body B → full no-op.
        // Must yield A = [B1, Cyl7, B3] and (by removal) B loses both prims.
        let target = [Prim(1)];
        let slot = resolve_prim_over_prim_row(&target, nid(1), 1, HeaderZone::Bottom);
        let PrimOverPrimRow::Insert { index, .. } = slot else { panic!("expected slot") };
        assert_eq!(index, 1);
        assert_eq!(apply_cross_body(&target, &[Prim(7), Prim(3)], index),
            vec![Prim(1), Prim(7), Prim(3)]);
    }

    #[test]
    fn between_slot_before_next_row_equals_after_this_row() {
        // Band partition sanity: Bottom of row i and Top of row i+1 resolve to the SAME slot.
        let order = [Prim(7), Prim(4), Prim(6)];
        let a = resolve_prim_over_prim_row(&order, nid(1), 4, HeaderZone::Bottom);
        let b = resolve_prim_over_prim_row(&order, nid(1), 6, HeaderZone::Top);
        assert_eq!(a, b);
    }

    #[test]
    fn bottom_third_of_last_subbody_is_valid_end_slot() {
        // Hovering the bottom third of the LAST row (a sub-body at the end of the parent's
        // order, position len-1) resolves to index == len: the valid end slot, not an
        // out-of-range index. Parent order: [Prim, Prim, Body] → Body at pos 2, len 3.
        let r = resolve_prim_over_body_header(nid(10), Some((nid(1), 2)), HeaderZone::Bottom);
        assert_eq!(r, PrimOverBodyHeader::InsertIntoOrder { body: nid(1), index: 3 });
        // slot math accepts index == len cleanly (anchor None → append).
        let order = [Prim(1), Prim(2), Body(nid(10))];
        assert_eq!(slot_anchor(&order, 3, &[Prim(99)]), None);
        assert_eq!(slot_insert_position(&order, Prim(99), None), 3);
    }
}
