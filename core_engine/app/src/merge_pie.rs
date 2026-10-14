//! Alt+M vertex Merge pie — Average / Last / By Distance.
//!
//! Interaction lives in `input_keyboard/handle.rs` using the **same hold-pie template**
//! as Z / boolean / T pies: open on key *press*, hover while held, confirm on key
//! *release* via `PieMenuState::release()`. Holding never re-triggers open or apply.

use crate::AzraApp;

impl AzraApp {
    /// Slice idx → merge op. Layout matches `merge_pie_def` (3 equal slices).
    pub(crate) fn apply_merge_pie(&mut self, idx: usize) {
        match idx {
            0 => self.poly_merge_vertices(),         // Average / at center
            1 => self.poly_merge_vertices_at_last(), // Last (active vert)
            2 => self.poly_merge_by_distance(),      // By distance
            _ => {}
        }
    }
}
