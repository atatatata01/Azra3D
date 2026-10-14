// app_types/src/keymap/ — contextual keymap vocabulary (INPUT_ARCHITECTURE_PLAN.md).
//
// S1 of the gated migration: TYPES ONLY, zero dispatch callers, unit tests only. This module is inert
// — nothing reads it for input routing yet. Later slices wire it in (S2 declares `Guard` on actions,
// S4 lands the resolver + `build_ctx`). Keeping the types landed first means every later slice is a
// small, green, revertable diff.

pub mod ctx;
pub mod event;
pub mod decl;

pub use ctx::*;
pub use event::*;
pub use decl::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(surface: Surface, tool: ToolId, pillar: Pillar, work: WorkKind, elem: ElemMode) -> Ctx {
        Ctx { surface, tool, tool_modal: None, pillar, work, elem, mods: Mods::default(), text_focus: false }
    }

    #[test]
    fn any_accepts_every_context() {
        // The 44 `context: None` actions become When::ANY — must accept ANY live context.
        let contexts = [
            ctx(Surface::Viewport, ToolId::Idle, Pillar::Sdf, WorkKind::Object, ElemMode::NA),
            ctx(Surface::Outliner, ToolId::PolyBevel, Pillar::Polygonal, WorkKind::Edit, ElemMode::Edge),
            ctx(Surface::Unknown, ToolId::Pie(PieId::Symmetry), Pillar::Voxel, WorkKind::Sculpt, ElemMode::Face),
        ];
        for c in contexts { assert!(When::ANY.accepts(&c), "ANY must accept {:?}", c); }
    }

    #[test]
    fn accepts_is_conjunctive_across_axes() {
        let w = When { surface: SurfaceMask::only(Surface::Viewport), tool: ToolMask::ANY,
                       pillar: PillarMask::only(Pillar::Polygonal), work: WorkMask::only(WorkKind::Edit),
                       elem: ElemMask::only(ElemMode::Vertex).or(ElemMode::Edge) };
        // Matches: viewport + polygonal + edit + vertex.
        assert!(w.accepts(&ctx(Surface::Viewport, ToolId::Idle, Pillar::Polygonal, WorkKind::Edit, ElemMode::Vertex)));
        // Wrong surface → no.
        assert!(!w.accepts(&ctx(Surface::Outliner, ToolId::Idle, Pillar::Polygonal, WorkKind::Edit, ElemMode::Vertex)));
        // Wrong elem (Face not in {Vertex,Edge}) → no.
        assert!(!w.accepts(&ctx(Surface::Viewport, ToolId::Idle, Pillar::Polygonal, WorkKind::Edit, ElemMode::Face)));
    }

    #[test]
    fn overlap_is_sound_arithmetic() {
        // Two per-tool actions on the SAME chord in DISJOINT tools must NOT be reported as conflicting
        // (bevel.clamp_toggle vs loopcut.clamp_toggle, both default C). This is the whole point.
        let bevel = When { tool: ToolMask::only(ToolId::PolyBevel), ..When::ANY };
        let loopcut = When { tool: ToolMask::only(ToolId::PolyLoopCutSlide), ..When::ANY };
        assert!(!bevel.overlaps(&loopcut), "disjoint tools must not overlap");
        assert!(bevel.overlaps(&When::ANY), "ANY overlaps everything");
        assert!(bevel.overlaps(&bevel));
    }

    #[test]
    fn layer_derived_and_totally_ordered() {
        assert_eq!(layer_of(&When { tool: ToolMask::only(ToolId::Bevel), ..When::ANY }), Layer::Tool);
        assert_eq!(layer_of(&When { pillar: PillarMask::only(Pillar::Polygonal), ..When::ANY }), Layer::Mode);
        assert_eq!(layer_of(&When { surface: SurfaceMask::only(Surface::Outliner), ..When::ANY }), Layer::Surface);
        assert_eq!(layer_of(&When::ANY), Layer::Global);
        // Total order: Tool most-specific → Global least. resolve() picks the smallest.
        assert!(Layer::Tool < Layer::Mode && Layer::Mode < Layer::Surface && Layer::Surface < Layer::Global);
    }

    #[test]
    fn tool_capture_classifies_modals_exclusive() {
        // Modal drags swallow unmatched keys; passive tools do not.
        assert!(matches!(ToolId::PolyBevel.capture(), Capture::Exclusive));
        assert!(matches!(ToolId::Transform.capture(), Capture::Exclusive));
        assert!(matches!(ToolId::Pie(PieId::F).capture(), Capture::Exclusive));
        assert!(matches!(ToolId::Idle.capture(), Capture::Additive));
        assert!(matches!(ToolId::Gizmo.capture(), Capture::Additive));
        assert!(matches!(ToolId::PolyBoxSelect.capture(), Capture::Additive));
    }

    #[test]
    fn tool_idx_unique_and_fits_u32() {
        // Every ToolId bit is distinct (mask correctness depends on it). 29 indices, all < 32.
        let ids = [
            ToolId::Idle, ToolId::Transform, ToolId::Gizmo, ToolId::FacePushPull, ToolId::Bevel,
            ToolId::BoolEdgeBevel, ToolId::FalloffAdjust, ToolId::Create, ToolId::SmartMirror,
            ToolId::PivotEdit, ToolId::SelectionBox, ToolId::PolyExtrude, ToolId::PolyEdgeExtrude,
            ToolId::PolyInset,
            ToolId::PolyBoxSelect, ToolId::PolyCircleSelect, ToolId::PolyPaintSelect, ToolId::PolyRegionSelect,
            ToolId::PolyTransform, ToolId::PolyBevel, ToolId::PolyLoopCutSlide, ToolId::PolyVertSlide,
            ToolId::PolyEdgeSlide, ToolId::PolyFaceHingeRotate, ToolId::PolyShrinkFatten,
            ToolId::NurbsCvMove, ToolId::BlendKDrag,
            ToolId::ClipCurveDraw, ToolId::CurvePointTransform, ToolId::CurveBevelTransform,
            ToolId::SpinePointEdit, ToolId::Pie(PieId::F),
        ];
        let mut seen = 0u32;
        for id in ids {
            let i = id.idx();
            assert!(i < 32, "idx {} out of range", i);
            assert_eq!(seen & (1 << i), 0, "duplicate idx {}", i);
            seen |= 1 << i;
        }
        // All pies share the one Pie bit.
        assert_eq!(ToolId::Pie(PieId::F).idx(), ToolId::Pie(PieId::Symmetry).idx());
    }
}
