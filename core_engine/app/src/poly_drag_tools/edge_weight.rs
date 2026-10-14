// edge_weight.rs — live authored edge-weight drag (`W`).
//
// Owner spec 2026-08-11: *"pressing W could let set weithed edge, if poly is selected boundery get
// weited edge, clic drag let you [set] the strengh, shift lock to 20, 40, 60, 80, 100 pourcent,
// ctrl reset the weitg to 0"* + *"right clic accept weithed"*.
//
// The DATA half already existed and is untouched here: `CustomData.crease` (per-edge 0..1, keyed by
// canonical `EdgeKey`), persisted in `.fl4` v8, re-keyed by position across rebuilds, read by
// `GeoChannelKind::EdgeWeight` and by the dissolve / Mesh Cleanup protection rules, and drawn by the
// magenta "Edge Weights" overlay whose intensity + width already track the value. This file adds the
// interactive AUTHORING half; the write itself goes through `poly_write_edge_weights`, the same one
// path the menu presets use.

use eframe::egui;
use core_math::poly_mesh::EdgeKey;
use crate::{AzraApp, PolyEdgeWeightDrag, ActiveTool};

/// Screen pixels of horizontal drag that span the whole 0..1 weight range.
const DRAG_SPAN_PX: f32 = 300.0;

/// Live granularity: 1 %. Coarse enough that a drag writes ~100 distinct values instead of one per
/// pixel per frame, fine enough to feel continuous — and it makes the Shift stops land EXACTLY on
/// 20/40/60/80/100 instead of 0.19999.
const STEP: f32 = 0.01;

/// Shift snap stops — 20 % increments, 0 included so a snapped drag can reach "cleared".
const SNAP: f32 = 0.2;

/// Drag → weight. `dx` is horizontal pixels since the segment started, `base` the weight it started
/// from; right = stronger. `shift` locks to the 20 % stops (owner: "shift lock to 20, 40, 60, 80, 100
/// pourcent"), otherwise the value quantizes to 1 %.
///
/// Pure so the stop contract is testable without an app: the whole point of `shift` is that the value
/// lands EXACTLY on 0.2/0.4/0.6/0.8/1.0, not on 0.19999.
fn weight_from_drag(base: f32, dx: f32, shift: bool) -> f32 {
    let raw = base + dx / DRAG_SPAN_PX;
    let step = if shift { SNAP } else { STEP };
    ((raw / step).round() * step).clamp(0.0, 1.0)
}

impl AzraApp {
    /// Arm the `W` edge-weight drag on the current selection.
    ///
    /// Targets come from the shared [`poly_edge_weight_targets`](AzraApp::poly_edge_weight_targets)
    /// resolver (Edge mode = selected edges, Face/Group = region boundary), so the key and the menu
    /// presets can never disagree about what "the selected edges" means.
    ///
    /// The drag starts from the HIGHEST weight already on the target set rather than from 0, so
    /// pressing `W` again on a creased edge continues from where it was instead of wiping it.
    pub(crate) fn poly_start_edge_weight_drag(&mut self, mouse: egui::Pos2) {
        let Some((nid, keys)) = self.poly_edge_weight_targets("Edge Weight") else {
            return;
        };
        // Pre-drag value per edge: `None` = no entry at all (≠ an entry of 0.0). Cancel replays this.
        let (entries, base) = {
            let cd = self.poly_cd(nid);
            let mut entries: Vec<(EdgeKey, Option<f32>)> = Vec::with_capacity(keys.len());
            let mut base = 0.0f32;
            for k in keys {
                let cur = cd.and_then(|c| c.crease.get(k).copied());
                base = base.max(cur.unwrap_or(0.0));
                entries.push((k, cur));
            }
            (entries, base.clamp(0.0, 1.0))
        };
        let n = entries.len();
        self.begin_episode("Set Edge Weight");
        self.start_operation(ActiveTool::PolyEdgeWeight(PolyEdgeWeightDrag {
            node_id: nid,
            keys: entries,
            start_mouse: mouse,
            base,
            weight: base,
            num_buf: String::new(),
            ctrl_was: false,
        }));
        // No feedback without it — the overlay ships off.
        self.poly_show_edge_weight_overlay();
        self.toast(&format!(
            "Edge Weight ({n} edge{}): drag = strength · type a % · Shift = 20% steps · Ctrl = reset 0 · click/Enter accept · RMB/Esc cancel",
            if n == 1 { "" } else { "s" }
        ));
    }

    /// Per-frame update — a FREE modal: the weight follows the bare cursor from the moment `W` armed,
    /// no button held (owner 2026-08-11: *"press W, drag, enter value, clic accept, righ clic
    /// cancel"*). A click therefore never has to double as "begin the drag", which is what frees it to
    /// be the accept — the same shape as Inset / Extrude / Offset Edge Loop.
    ///
    /// `shift` snaps to the 20 % stops; `ctrl` resets to 0 on its RISING EDGE only (a held-Ctrl reset
    /// would pin the value at 0 and leave the tool inert while Ctrl is down). A typed value in
    /// `num_buf` wins over the cursor until it is backspaced empty.
    pub(crate) fn poly_update_edge_weight_drag(&mut self, mouse: egui::Pos2, shift: bool, ctrl: bool) {
        let Some(d) = self.tool.current.as_poly_edge_weight_mut() else { return };
        let nid = d.node_id;
        let prev = d.weight;

        // Ctrl rising edge = RESET to 0, and re-anchor so continuing the drag climbs from 0. Also
        // drops any typed value — "reset" that left a number on screen would be a lie.
        let ctrl_reset = ctrl && !d.ctrl_was;
        d.ctrl_was = ctrl;
        if ctrl_reset {
            d.weight = 0.0;
            d.base = 0.0;
            d.start_mouse = mouse;
            d.num_buf.clear();
        }

        if !ctrl_reset {
            if let Ok(pct) = d.num_buf.parse::<f32>() {
                // Typed value owns the weight (quantised the same way, so 33.7 stores as 0.34 and the
                // readout cannot disagree with what was stored).
                d.weight = ((pct / 100.0 / STEP).round() * STEP).clamp(0.0, 1.0);
            } else if d.num_buf.is_empty() {
                d.weight = weight_from_drag(d.base, mouse.x - d.start_mouse.x, shift);
            }
            // A partial buffer ("." or "-") parses as neither — hold the last value rather than
            // snapping back to the cursor mid-typing.
        }

        let w = d.weight;
        if (w - prev).abs() < STEP * 0.5 {
            return; // no visible change — do not re-run the display stack for nothing
        }
        let value = if w <= 0.0 { None } else { Some(w) };
        let entries: Vec<(EdgeKey, Option<f32>)> = {
            let d = match self.tool.current.as_poly_edge_weight() { Some(d) => d, None => return };
            d.keys.iter().map(|&(k, _)| (k, value)).collect()
        };
        self.poly_write_edge_weights(nid, &entries);
    }

    /// Typed numeric entry, in PERCENT ("40" → 0.4). `\x08` = backspace; emptying the buffer hands the
    /// weight back to the cursor. Same shape as the Extrude Sweep angle entry so the two modals type
    /// alike. The write goes through the normal update so there is ONE place that maps state → sidecar.
    pub(crate) fn poly_edge_weight_num_input(&mut self, ch: char) {
        let ctrl_latch = {
            let Some(d) = self.tool.current.as_poly_edge_weight_mut() else { return };
            if ch == '\x08' {
                d.num_buf.pop();
            } else if ch.is_ascii_digit() || ch == '.' {
                // No '-': a negative weight is meaningless (the column is 0..1) and typing one would
                // silently clamp to 0, which reads as "the keystroke did nothing".
                if d.num_buf.len() < 6 {
                    d.num_buf.push(ch);
                }
            } else {
                return;
            }
            d.ctrl_was
        };
        let mouse = self.viewport.viewport_mouse;
        self.poly_update_edge_weight_drag(mouse, false, false);
        // `key_pressed` ignores modifiers, so a digit typed with Ctrl DOWN reaches here. Passing
        // `ctrl: false` above would clear the rising-edge latch, and the next real frame — Ctrl still
        // held — would read as a fresh press and wipe the number that was just typed. Restore it.
        if let Some(d) = self.tool.current.as_poly_edge_weight_mut() {
            d.ctrl_was = ctrl_latch;
        }
    }

    /// Accept — ONE undo entry for the whole gesture. LMB-click, Enter and tool-switch all land here
    /// (RMB CANCELS: owner 2026-08-11, standard Tool Exit Convention). A gesture that ended on the
    /// value it started from commits nothing (no dead undo entry).
    pub(crate) fn poly_confirm_edge_weight_drag(&mut self) {
        let Some(d) = self.tool.current.as_poly_edge_weight() else { return };
        let w = d.weight;
        let n = d.keys.len();
        let value = if w <= 0.0 { None } else { Some(w) };
        let unchanged = d.keys.iter().all(|&(_, orig)| match (orig, value) {
            (None, None) => true,
            (Some(a), Some(b)) => (a - b).abs() < STEP * 0.5,
            _ => false,
        });
        if unchanged {
            self.poly_cancel_edge_weight_drag();
            return;
        }
        // Write the accepted value ONCE more before committing, so the accept is authoritative rather
        // than trusting that some earlier frame happened to write it. It normally is a no-op (the drag
        // already wrote this value), but it is NOT when the selection started with MIXED weights and
        // the user accepted without dragging: `weight` then still holds the set's max, the live path
        // had nothing to write (no change to itself), and committing alone would push an undo entry
        // for a uniform value that was never applied.
        let nid = d.node_id;
        let entries: Vec<(EdgeKey, Option<f32>)> = d.keys.iter().map(|&(k, _)| (k, value)).collect();
        self.poly_write_edge_weights(nid, &entries);
        self.commit_episode();
        self.poly_flush_mesh_dirty();
        self.tool.current = ActiveTool::Idle;
        if w <= 0.0 {
            self.toast(&format!("Cleared weight on {n} edge{}", if n == 1 { "" } else { "s" }));
        } else {
            self.toast(&format!(
                "Edge weight {pct:.0}% on {n} edge{}",
                if n == 1 { "" } else { "s" },
                pct = w * 100.0
            ));
        }
    }

    /// Cancel — replay each edge's PRE-DRAG entry (including "had no entry at all") and drop the
    /// episode. Restores from the drag's own snapshot rather than from the undo entry's whole-sidecar
    /// clone, so an unrelated body's authored state can never be collateral damage.
    pub(crate) fn poly_cancel_edge_weight_drag(&mut self) {
        let Some(d) = self.tool.current.as_poly_edge_weight() else { return };
        let nid = d.node_id;
        let entries: Vec<(EdgeKey, Option<f32>)> = d.keys.clone();
        self.poly_write_edge_weights(nid, &entries);
        self.cancel_episode();
        self.poly_flush_mesh_dirty();
        self.tool.current = ActiveTool::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner-specified Shift stops must be EXACT — a masked modifier compares against these
    /// values, and a readout of "20%" that stores 0.19999 is a lie the user cannot correct by hand.
    #[test]
    fn shift_locks_to_twenty_percent_stops() {
        for (i, expect) in [0.0f32, 0.2, 0.4, 0.6, 0.8, 1.0].iter().enumerate() {
            // Land mid-way INTO each bucket so rounding — not luck — puts us on the stop.
            let dx = (i as f32 * SNAP + SNAP * 0.3) * DRAG_SPAN_PX;
            let w = weight_from_drag(0.0, dx, true);
            assert!(
                (w - expect).abs() < 1e-6,
                "shift drag of {dx}px should snap to {expect}, got {w}"
            );
        }
    }

    #[test]
    fn drag_is_clamped_to_unit_range_both_ways() {
        assert_eq!(weight_from_drag(0.5, 10_000.0, false), 1.0);
        assert_eq!(weight_from_drag(0.5, -10_000.0, false), 0.0);
        assert_eq!(weight_from_drag(0.5, -10_000.0, true), 0.0);
    }

    /// Right = stronger, left = weaker, and a full-span drag covers the whole range (so the tool is
    /// reachable end to end without re-dragging).
    #[test]
    fn full_span_drag_covers_the_range() {
        assert!(weight_from_drag(0.0, DRAG_SPAN_PX * 0.5, false) > 0.45);
        assert_eq!(weight_from_drag(0.0, DRAG_SPAN_PX, false), 1.0);
        assert_eq!(weight_from_drag(1.0, -DRAG_SPAN_PX, false), 0.0);
    }

    /// Continuing a drag from a latched value must not jump: the base is the value, not zero.
    #[test]
    fn base_is_the_starting_value() {
        assert!((weight_from_drag(0.6, 0.0, false) - 0.6).abs() < 1e-6);
        assert!((weight_from_drag(0.6, DRAG_SPAN_PX * 0.2, false) - 0.8).abs() < 1e-6);
    }
}
