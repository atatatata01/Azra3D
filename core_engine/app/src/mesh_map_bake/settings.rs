//! Bake settings model (G1). Persisted as the `FL4BAKE` tail — never in the bincode blob.
//!
//! Wire shape is integers + `Vec<u64>` so we never serde `MeshMapKind` as a map key and
//! `BakeSource` is append-only via `source_kind` (0 = SelfMesh, 1 = HighPoly).

use super::MeshMapKind;
use core_math::scene_graph::NodeId;

/// Where the bake samples from. G2 fills HighPoly; G1 only authors the choice.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BakeSource {
    SelfMesh,
    HighPoly { nids: Vec<NodeId>, match_by_name: bool },
}

impl BakeSource {
    pub fn kind(self: &Self) -> u32 {
        match self {
            BakeSource::SelfMesh => 0,
            BakeSource::HighPoly { .. } => 1,
        }
    }

    pub fn from_wire(kind: u32, nids: Vec<u64>, match_by_name: bool) -> Self {
        match kind {
            1 => BakeSource::HighPoly {
                nids: nids.into_iter().map(NodeId).collect(),
                match_by_name,
            },
            _ => BakeSource::SelfMesh,
        }
    }
}

impl Default for BakeSource {
    fn default() -> Self {
        BakeSource::SelfMesh
    }
}

/// Per-body bake knobs. Global resolution only (D8) — no per-map override in v1.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BakeSettings {
    pub enabled_maps: u32,
    pub res_global: u32,
    /// 1 | 2 | 4.
    pub supersample: u32,
    /// Island padding in output texels.
    pub padding_px: u32,
    /// G7: 16-bit dual-plane for Position / ObjectNormal / TangentNormal / BentNormal.
    pub bit16: bool,
    pub source: BakeSource,
    /// Numeric cage (G2): ray distance along / against the low normal.
    pub max_front: f32,
    pub max_rear: f32,
    /// G7 mesh cage: scene bodies used as the projection envelope. Empty = numeric only.
    pub cage_nids: Vec<NodeId>,
    /// G7: bake each occupied UDIM tile as its own image.
    pub udim: bool,
    /// G7: AO / Thickness / Bent on a worker wgpu device. Falls back to CPU.
    pub gpu_rays: bool,
    pub ao_rays: u32,
    pub curv_radius: f32,
}

impl Default for BakeSettings {
    fn default() -> Self {
        Self {
            enabled_maps: u32::MAX,
            res_global: 1024,
            supersample: 1,
            padding_px: 0,
            bit16: false,
            source: BakeSource::SelfMesh,
            max_front: 0.1,
            max_rear: 0.05,
            cage_nids: Vec::new(),
            udim: false,
            gpu_rays: false,
            ao_rays: 64,
            curv_radius: 0.0,
        }
    }
}

impl BakeSettings {
    pub fn clamp(mut self) -> Self {
        self.res_global = self.res_global.clamp(64, 4096);
        self.supersample = match self.supersample {
            2 | 4 => self.supersample,
            _ => 1,
        };
        self.padding_px = self.padding_px.min(64);
        self.max_front = self.max_front.max(0.0);
        self.max_rear = self.max_rear.max(0.0);
        self.ao_rays = self.ao_rays.clamp(8, 512);
        self.curv_radius = self.curv_radius.max(0.0);
        self
    }

    pub fn maps(&self) -> Vec<MeshMapKind> {
        MeshMapKind::ALL
            .iter()
            .copied()
            .filter(|k| self.enabled_maps & k.bit() != 0)
            .collect()
    }

    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

impl crate::AzraApp {
    pub(crate) fn bake_settings_for(&self, body: NodeId) -> BakeSettings {
        self.ui
            .mesh_map_settings
            .get(&body)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn store_bake_settings(&mut self, body: NodeId, settings: BakeSettings) {
        let s = settings.clamp();
        if s.is_default() {
            self.ui.mesh_map_settings.remove(&body);
        } else {
            self.ui.mesh_map_settings.insert(body, s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_enables_every_current_bit() {
        let s = BakeSettings::default();
        assert_eq!(s.maps().len(), MeshMapKind::ALL.len());
        assert!(s.is_default());
    }

    #[test]
    fn source_kind_is_append_only() {
        assert_eq!(BakeSource::SelfMesh.kind(), 0);
        assert_eq!(
            BakeSource::HighPoly {
                nids: vec![],
                match_by_name: false
            }
            .kind(),
            1
        );
        // Unknown future kinds degrade to SelfMesh, never panic.
        assert_eq!(BakeSource::from_wire(99, vec![1], true), BakeSource::SelfMesh);
    }

    #[test]
    fn clamp_rejects_illegal_res_and_ss() {
        let mut s = BakeSettings::default();
        s.res_global = 7;
        s.supersample = 3;
        s.ao_rays = 2;
        let s = s.clamp();
        assert_eq!(s.res_global, 64);
        assert_eq!(s.supersample, 1);
        assert_eq!(s.ao_rays, 8);
    }
}
