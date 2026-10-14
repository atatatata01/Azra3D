//! UV-viewport texture underlay (G5).
//!
//! The UV split (`poly/uv.rs`) paints a cached 256² preview of `slot_rgba` (or a
//! checker). **Never** `upload_map` from the draw path — D8: one shared
//! `layer_res`; the underlay is a fixed-size egui texture, rebuilt only when the
//! slot or array generation changes.

use crate::material_textures::{map_side_len, MaterialTextureArray};

/// Fixed preview edge. Independent of `MaterialTextures.layer_res` (D8).
pub(crate) const UNDERLAY_RES: usize = 256;
const CHECKER_CELLS: usize = 8;

/// What the UV square shows. Runtime viewport state — not persisted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum UvUnderlaySource {
    #[default]
    Checker,
    /// Material-panel selected Image layer on the active body's material.
    SelectedLayer,
    /// [`crate::mesh_map_bake::BakedImage`] registry name.
    BakedImage(String),
    /// Texture-array slot display name (`slot_name`).
    MaterialSlot(String),
}

impl UvUnderlaySource {
    pub fn label(&self) -> String {
        match self {
            UvUnderlaySource::Checker => "Checker".into(),
            UvUnderlaySource::SelectedLayer => "Selected layer".into(),
            UvUnderlaySource::BakedImage(n) => format!("Bake: {n}"),
            UvUnderlaySource::MaterialSlot(n) => format!("Map: {n}"),
        }
    }
}

/// Resolved paint target. `Missing` falls back to checker + a badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UvUnderlayResolved {
    Checker,
    Slot(u32),
    Missing,
}

/// Pure resolve — no pixels, no GPU. GATE: every source has a unit test.
pub(crate) fn resolve_uv_underlay(
    source: &UvUnderlaySource,
    selected_layer_slot: Option<u32>,
    baked_slot: Option<u32>,
    material_slot: Option<u32>,
) -> UvUnderlayResolved {
    let slot = match source {
        UvUnderlaySource::Checker => return UvUnderlayResolved::Checker,
        UvUnderlaySource::SelectedLayer => selected_layer_slot,
        UvUnderlaySource::BakedImage(_) => baked_slot,
        UvUnderlaySource::MaterialSlot(_) => material_slot,
    };
    match slot {
        Some(s) if s > 0 => UvUnderlayResolved::Slot(s),
        _ => UvUnderlayResolved::Missing,
    }
}

/// egui-texture cache. Hit = no pixel alloc. Miss = `UNDERLAY_RES²` downsample.
pub(crate) struct UvUnderlayCache {
    checker: Option<eframe::egui::TextureHandle>,
    slot: Option<(u32, u64, eframe::egui::TextureHandle)>,
}

impl UvUnderlayCache {
    pub fn new() -> Self {
        Self {
            checker: None,
            slot: None,
        }
    }

    pub fn texture(
        &mut self,
        ctx: &eframe::egui::Context,
        resolved: UvUnderlayResolved,
        arr: Option<&MaterialTextureArray>,
    ) -> eframe::egui::TextureId {
        match resolved {
            UvUnderlayResolved::Slot(s) => {
                if let Some(arr) = arr {
                    if let Some(id) = self.slot_texture(ctx, arr, s) {
                        return id;
                    }
                }
                self.checker_texture(ctx)
            }
            UvUnderlayResolved::Checker | UvUnderlayResolved::Missing => {
                self.checker_texture(ctx)
            }
        }
    }

    fn checker_texture(&mut self, ctx: &eframe::egui::Context) -> eframe::egui::TextureId {
        if let Some(h) = &self.checker {
            return h.id();
        }
        let px = checker_rgba(UNDERLAY_RES, CHECKER_CELLS);
        let ci = eframe::egui::ColorImage::from_rgba_unmultiplied(
            [UNDERLAY_RES, UNDERLAY_RES],
            &px,
        );
        let h = ctx.load_texture(
            "uv_underlay_checker",
            ci,
            eframe::egui::TextureOptions::NEAREST,
        );
        let id = h.id();
        self.checker = Some(h);
        id
    }

    fn slot_texture(
        &mut self,
        ctx: &eframe::egui::Context,
        arr: &MaterialTextureArray,
        slot: u32,
    ) -> Option<eframe::egui::TextureId> {
        let gen = arr.generation;
        if let Some((s, g, h)) = &self.slot {
            if *s == slot && *g == gen {
                return Some(h.id());
            }
        }
        let src = arr.slot_rgba(slot)?;
        let px = downsample_underlay(src);
        let ci = eframe::egui::ColorImage::from_rgba_unmultiplied(
            [UNDERLAY_RES, UNDERLAY_RES],
            &px,
        );
        let h = ctx.load_texture(
            format!("uv_underlay_{slot}"),
            ci,
            eframe::egui::TextureOptions::LINEAR,
        );
        let id = h.id();
        self.slot = Some((slot, gen, h));
        Some(id)
    }
}

pub(crate) fn checker_rgba(res: usize, cells: usize) -> Vec<u8> {
    let mut px = vec![0u8; res * res * 4];
    let cell = (res / cells.max(1)).max(1);
    for y in 0..res {
        for x in 0..res {
            let c = ((x / cell) + (y / cell)) % 2;
            let v = if c == 0 { 72u8 } else { 98 };
            let i = (y * res + x) * 4;
            px[i] = v;
            px[i + 1] = v;
            px[i + 2] = v;
            px[i + 3] = 255;
        }
    }
    px
}

/// Stride box-sample to [`UNDERLAY_RES`]. O(UNDERLAY_RES²), never O(layer_res²).
pub(crate) fn downsample_underlay(src: &[u8]) -> Vec<u8> {
    let Some(map_res) = map_side_len(src) else {
        return checker_rgba(UNDERLAY_RES, CHECKER_CELLS);
    };
    let stride = (map_res as usize / UNDERLAY_RES).max(1);
    let mut out = vec![0u8; UNDERLAY_RES * UNDERLAY_RES * 4];
    for ty in 0..UNDERLAY_RES {
        for tx in 0..UNDERLAY_RES {
            let sx = (tx * stride + stride / 2).min(map_res as usize - 1);
            let sy = (ty * stride + stride / 2).min(map_res as usize - 1);
            let si = (sy * map_res as usize + sx) * 4;
            let di = (ty * UNDERLAY_RES + tx) * 4;
            if si + 3 < src.len() {
                out[di..di + 4].copy_from_slice(&src[si..si + 4]);
            }
        }
    }
    out
}

impl crate::AzraApp {
    pub(crate) fn resolve_uv_underlay_live(
        &self,
        body: Option<core_math::scene_graph::NodeId>,
    ) -> UvUnderlayResolved {
        let src = &self.ui.uv_underlay;
        let selected = body.and_then(|b| self.selected_layer_tex_slot(b));
        let baked = match src {
            UvUnderlaySource::BakedImage(n) => self
                .ui
                .baked_images
                .by_name(n)
                .and_then(|e| {
                    if e.pixels_missing || e.slot == 0 {
                        None
                    } else {
                        Some(e.slot)
                    }
                }),
            _ => None,
        };
        let material = match src {
            UvUnderlaySource::MaterialSlot(n) => self.material_slot_by_name(n),
            _ => None,
        };
        resolve_uv_underlay(src, selected, baked, material)
    }

    fn selected_layer_tex_slot(
        &self,
        body: core_math::scene_graph::NodeId,
    ) -> Option<u32> {
        use crate::material_panel::model::LayerContent;
        let mid = self.poly_body_graph_material(body)?;
        let (kind_u, li) = *self.ui.mp_layer_sel.get(&mid)?;
        let doc = self.ui.material_panel_docs.get(&mid)?;
        let ch = doc.channels.iter().find(|c| c.kind as u32 == kind_u)?;
        let layer = ch.layers.get(li)?;
        match layer.content {
            LayerContent::Image { tex_id, .. } if tex_id > 0 => Some(tex_id),
            _ => None,
        }
    }

    fn poly_body_graph_material(
        &self,
        body: core_math::scene_graph::NodeId,
    ) -> Option<u32> {
        use core_math::scene_graph::NodePayload;
        match &self.scene.scene_graph.get(body)?.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh
                .faces
                .iter()
                .find(|f| f.material_id > 0)
                .map(|f| f.material_id),
            _ => None,
        }
    }

    fn material_slot_by_name(&self, name: &str) -> Option<u32> {
        let arr = &self.gpu.as_ref()?.material_textures;
        for s in 1..arr.used_layers() {
            if arr.slot_name(s) == Some(name) {
                return Some(s);
            }
        }
        None
    }

    pub(crate) fn uv_underlay_options(&self) -> Vec<UvUnderlaySource> {
        let mut out = vec![
            UvUnderlaySource::Checker,
            UvUnderlaySource::SelectedLayer,
        ];
        for img in &self.ui.baked_images.images {
            out.push(UvUnderlaySource::BakedImage(img.name.clone()));
        }
        if let Some(gpu) = self.gpu.as_ref() {
            let arr = &gpu.material_textures;
            for s in 1..arr.used_layers() {
                if let Some(n) = arr.slot_name(s) {
                    if self.ui.baked_images.by_name(n).is_none() {
                        out.push(UvUnderlaySource::MaterialSlot(n.to_string()));
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_checker() {
        assert_eq!(
            resolve_uv_underlay(&UvUnderlaySource::Checker, Some(3), Some(4), Some(5)),
            UvUnderlayResolved::Checker
        );
    }

    #[test]
    fn resolve_selected_layer_slot() {
        assert_eq!(
            resolve_uv_underlay(&UvUnderlaySource::SelectedLayer, Some(7), None, None),
            UvUnderlayResolved::Slot(7)
        );
    }

    #[test]
    fn resolve_selected_layer_missing() {
        assert_eq!(
            resolve_uv_underlay(&UvUnderlaySource::SelectedLayer, None, None, None),
            UvUnderlayResolved::Missing
        );
        assert_eq!(
            resolve_uv_underlay(&UvUnderlaySource::SelectedLayer, Some(0), None, None),
            UvUnderlayResolved::Missing
        );
    }

    #[test]
    fn resolve_baked_by_name() {
        let s = UvUnderlaySource::BakedImage("Cube.Curvature.1024".into());
        assert_eq!(
            resolve_uv_underlay(&s, None, Some(2), None),
            UvUnderlayResolved::Slot(2)
        );
        assert_eq!(
            resolve_uv_underlay(&s, None, None, None),
            UvUnderlayResolved::Missing
        );
    }

    #[test]
    fn resolve_material_slot_by_name() {
        let s = UvUnderlaySource::MaterialSlot("wood_albedo".into());
        assert_eq!(
            resolve_uv_underlay(&s, None, None, Some(9)),
            UvUnderlayResolved::Slot(9)
        );
        assert_eq!(
            resolve_uv_underlay(&s, None, None, None),
            UvUnderlayResolved::Missing
        );
    }

    /// GATE (G5 D8) — downsample is always UNDERLAY_RES, never the source layer_res.
    #[test]
    fn downsample_is_underlay_res_not_layer() {
        let side = 1024u32;
        let src = vec![80u8; (side * side * 4) as usize];
        let out = downsample_underlay(&src);
        assert_eq!(out.len(), UNDERLAY_RES * UNDERLAY_RES * 4);
        assert_ne!(out.len(), src.len());
    }

    #[test]
    fn checker_is_fixed_res() {
        let px = checker_rgba(UNDERLAY_RES, CHECKER_CELLS);
        assert_eq!(px.len(), UNDERLAY_RES * UNDERLAY_RES * 4);
        assert_eq!(px[3], 255);
    }

    #[test]
    fn labels_distinguish_sources() {
        assert_eq!(UvUnderlaySource::Checker.label(), "Checker");
        assert_eq!(UvUnderlaySource::SelectedLayer.label(), "Selected layer");
        assert_eq!(
            UvUnderlaySource::BakedImage("A".into()).label(),
            "Bake: A"
        );
        assert_eq!(
            UvUnderlaySource::MaterialSlot("B".into()).label(),
            "Map: B"
        );
    }
}
