//! WeldMap — position-keyed vertex identity (BEVEL.md §5 invariant 2).
//!
//! STEP 2 weld of rail endpoints and every subsequent corner/arc/cap vertex goes through
//! `WeldMap::push`. `from_polygons` twins by index; coincident points must share one index.

use glam::Vec3;
use std::collections::HashMap;

/// Position-welded vertex accumulator — the structural fix for the multi-edge corner tear.
///
/// `from_polygons` welds half-edges by VERTEX INDEX, never by coordinate. So any two corner points that
/// must be the SAME physical vertex (where parallel rails of adjacent beveled edges meet) MUST be handed
/// the SAME index, or the rebuilt mesh tears into a non-manifold seam. `push` quantizes to a fixed grid:
/// coincident points collapse to one index; distinct points never collide. EVERY offset/corner vertex
/// the algorithm emits goes through `push` — indices are never hand-assigned.
pub(crate) struct WeldMap {
    pub positions: Vec<Vec3>,
    map: HashMap<(i64, i64, i64), usize>,
}

impl WeldMap {
    pub fn new() -> Self { Self { positions: Vec::new(), map: HashMap::new() } }

    /// 1e-5 quantization grid — finer than any meaningful modelling tolerance, coarse enough that
    /// genuinely-coincident floating-point results (e.g. two rails computed from the same endpoint) map
    /// to one cell.
    fn key(p: Vec3) -> (i64, i64, i64) {
        const Q: f32 = 1.0e5;
        ((p.x * Q).round() as i64, (p.y * Q).round() as i64, (p.z * Q).round() as i64)
    }

    /// Return the index for `p`, reusing an existing welded vertex if one is within the grid cell.
    pub fn push(&mut self, p: Vec3) -> usize {
        let k = Self::key(p);
        if let Some(&i) = self.map.get(&k) { return i; }
        let i = self.positions.len();
        self.positions.push(p);
        self.map.insert(k, i);
        i
    }
}
