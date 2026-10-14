//! UDIM tile math (G7). Mari numbering: `1001 + u + 10*v`.

use super::source::PolyCurvSource;

/// First UDIM tile (U 0..1, V 0..1).
pub(crate) const UDIM_1001: u32 = 1001;
const MAX_TILES: usize = 20;
const TILE_MAX: i32 = 9;

/// Half-open tile index for a UV. Exact integers land on the previous tile
/// (`u=1.0` → tile 0) so a 0..1 unwrap is a single 1001.
pub(crate) fn uv_tile_coord(x: f32) -> i32 {
    if !x.is_finite() {
        return 0;
    }
    let f = x.floor();
    if x > 0.0 && (x - f) < 1.0e-5 {
        (f as i32) - 1
    } else {
        f as i32
    }
}

pub(crate) fn udim_index(tu: i32, tv: i32) -> Option<u32> {
    if tu < 0 || tv < 0 || tu > TILE_MAX || tv > TILE_MAX {
        return None;
    }
    Some(UDIM_1001 + tu as u32 + tv as u32 * 10)
}

pub(crate) fn udim_origin(index: u32) -> Option<(i32, i32)> {
    if index < UDIM_1001 {
        return None;
    }
    let o = (index - UDIM_1001) as i32;
    Some((o % 10, o / 10))
}

/// Unique occupied UDIM tiles, sorted, capped.
pub(crate) fn occupied_udim_tiles(src: &PolyCurvSource) -> Vec<(u32, i32, i32)> {
    use std::collections::BTreeSet;
    let mut set = BTreeSet::new();
    for uv in &src.corner_uv {
        let tu = uv_tile_coord(uv.x);
        let tv = uv_tile_coord(uv.y);
        if let Some(idx) = udim_index(tu, tv) {
            set.insert((idx, tu, tv));
        }
    }
    set.into_iter().take(MAX_TILES).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh_map_bake::source::{PolyCurvSource, PolySampleSpace};
    use core_math::mesh_curvature::RingMesh;
    use glam::{Vec2, Vec3};

    #[test]
    fn unit_unwrap_is_1001() {
        assert_eq!(uv_tile_coord(0.0), 0);
        assert_eq!(uv_tile_coord(0.99), 0);
        assert_eq!(uv_tile_coord(1.0), 0);
        assert_eq!(uv_tile_coord(1.01), 1);
        assert_eq!(udim_index(0, 0), Some(1001));
        assert_eq!(udim_index(1, 0), Some(1002));
        assert_eq!(udim_index(0, 1), Some(1011));
        assert_eq!(udim_origin(1002), Some((1, 0)));
    }

    #[test]
    fn occupied_two_islands() {
        let src = PolyCurvSource {
            rings: RingMesh {
                positions: vec![Vec3::ZERO; 8],
                face_start: vec![0, 4, 8],
                face_ring: vec![0, 1, 2, 3, 4, 5, 6, 7],
            },
            corner_uv: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(1.0, 1.0),
                Vec2::new(0.0, 1.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(2.0, 0.0),
                Vec2::new(2.0, 1.0),
                Vec2::new(1.0, 1.0),
            ],
            space: PolySampleSpace::default(),
        };
        let tiles = occupied_udim_tiles(&src);
        let ids: Vec<u32> = tiles.iter().map(|t| t.0).collect();
        assert_eq!(ids, vec![1001, 1002]);
    }
}
