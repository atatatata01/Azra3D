//! Tool category tables — honesty: `ready: true` only when ops/activate handle it.

use egui_phosphor::regular as ph;
use super::super::*; // ToolCatDef, ToolItem, ToolCmd, t/tk/tw/c/cw, ToolPillar

pub(crate) const SDF_CATS: &[ToolCatDef] = &[
    ToolCatDef {
        name: "Shapes", icon: ph::SHAPES,
        tools: &[
            t("Box", ph::CUBE), t("Sphere", ph::SPHERE), t("Cylinder", ph::CYLINDER),
            t("Cone", ph::TRIANGLE),
            ToolItem { name: "Torus", icon: ph::CIRCLE, key: "", ready: true, variants: true },
            t("Capsule", ph::CYLINDER), t("Plane", ph::SQUARE),
            t("Polyline", ph::LINE_SEGMENTS), t("Draw Polyline", ph::PENCIL_SIMPLE),
            t("Stair", ph::ROWS), t("Spiral Stair", ph::ARROWS_CLOCKWISE),
            t("Slice", ph::SCISSORS),
        ],
        cmds: &[
            cw("Swap Shape", ""), c("Duplicate", ""), c("Create Instance", ""),
            cw("Make Unique", ""), cw("Recenter Origin", ""),
        ],
    },
    ToolCatDef {
        name: "Boolean", icon: ph::INTERSECT,
        tools: &[
            tk("Union", ph::UNITE, ""), tk("Subtract", ph::SUBTRACT, ""),
            tk("Intersect", ph::INTERSECT, ""), tw("Blend K", ph::CIRCLE),
        ],
        cmds: &[
            cw("Cut Through", ""), c("Cycle Op", ""),
            cw("Flatten Booleans", ""), cw("Reverse Order", ""),
        ],
    },
    ToolCatDef {
        name: "Edge", icon: ph::DIAMOND,
        tools: &[
            tw("Bevel", ph::DIAMOND), tw("Bool Bevel", ph::PATH),
            tw("Seam", ph::LINE_SEGMENTS),
        ],
        cmds: &[cw("Rebuild Bevels", ""), cw("Clear Bevels", "")],
    },
    ToolCatDef {
        name: "Modify", icon: ph::WRENCH,
        tools: &[
            tw("Emboss", ph::ARROW_LINE_UP), tw("Twist", ph::WRENCH),
            tw("Taper", ph::WRENCH), tw("Shear", ph::WRENCH), tw("Displace", ph::WRENCH),
            tw("Smooth", ph::WAVE_SINE), tw("Spherize", ph::SPHERE), tw("Shell", ph::CUBE),
            tw("Clip Curve", ph::BEZIER_CURVE),
        ],
        cmds: &[cw("Bake Modifiers", ""), cw("Clear Modifiers", "")],
    },
    ToolCatDef {
        name: "Symmetry", icon: ph::FLIP_HORIZONTAL,
        tools: &[
            tw("Mirror", ph::FLIP_HORIZONTAL), tw("Smart Mirror", ph::FLIP_HORIZONTAL),
        ],
        cmds: &[
            cw("Toggle Symmetry", ""), cw("Symmetrize", ""),
            cw("Radial Array", ""),
        ],
    },
    ToolCatDef {
        name: "Transform", icon: ph::ARROWS_OUT_CARDINAL,
        tools: &[
            // Gizmo arm only (not full create ops) — still interactive
            tk("Move", ph::ARROWS_OUT_CARDINAL, ""), tk("Rotate", ph::CIRCLE, ""),
            tk("Scale", ph::CUBE, ""), tw("Elongate", ph::ARROW_LINE_UP),
            tw("Push/Pull", ph::POLYGON), tw("Pivot", ph::CROSSHAIR),
        ],
        cmds: &[cw("Reset Transform", ""), c("Focus Selected", "")],
    },
    ToolCatDef {
        name: "Convert", icon: ph::ARROWS_LEFT_RIGHT,
        tools: &[],
        cmds: &[
            cw("Convert to Mesh", ""), cw("Convert Poly to SDF", ""),
            cw("Link Poly as SDF", ""), c("Export Mesh", ""), cw("Bake Field", ""),
        ],
    },
];

pub(crate) const POLY_CATS: &[ToolCatDef] = &[
    ToolCatDef {
        name: "Create", icon: ph::PLUS_CIRCLE,
        tools: &[
            t("Cube", ph::CUBE), t("Sphere", ph::SPHERE), t("Cylinder", ph::CYLINDER),
            t("Cone", ph::TRIANGLE), t("Torus", ph::CIRCLE), t("Plane", ph::SQUARE),
            tw("Grid", ph::GRID_FOUR), tw("Circle", ph::CIRCLE),
        ],
        cmds: &[c("Join Meshes", ""), cw("Separate", "")],
    },
    ToolCatDef {
        name: "Vertex", icon: ph::DOTS_THREE_OUTLINE,
        tools: &[
            t("Merge", ph::ARROWS_IN), t("Merge by Distance", ph::ARROWS_IN),
            tw("Vertex Slide", ph::ARROWS_OUT_CARDINAL),
            // Production (BEVEL.md §11 2026-07-03 + 2026-08-09 app arm). Was `tw` (ready:false)
            // so the row was not clickable — owner "i can not bevel a vertex".
            tk("Bevel Vertices", ph::DIAMOND, "Ctrl+B"),
            tw("Rip", ph::DOTS_THREE), tw("Connect Path", ph::LINK),
        ],
        cmds: &[
            cw("New Edge/Face", ""), c("Dissolve Vertices", ""),
            c("Delete Vertices", ""), cw("Smooth Vertices", ""),
        ],
    },
    ToolCatDef {
        name: "Edge", icon: ph::LINE_SEGMENTS,
        tools: &[
            t("Loop Cut", ph::ROWS), t("Bevel", ph::DIAMOND),
            tw("Knife", ph::KNIFE), tw("Edge Slide", ph::ARROWS_OUT_CARDINAL),
            tw("Rotate Edge", ph::ARROWS_CLOCKWISE),
        ],
        cmds: &[
            c("Bridge Loops", ""), c("Dissolve Edges", ""), c("Delete Edges", ""),
            c("Toggle Seam", ""), c("Mark Seam", ""), c("Clear Seam", ""),
        ],
    },
    ToolCatDef {
        name: "Face", icon: ph::POLYGON,
        tools: &[
            t("Extrude", ph::ARROW_LINE_UP), t("Inset", ph::SELECTION),
            tw("Poke", ph::POLYGON), tw("Spin", ph::ARROWS_CLOCKWISE),
        ],
        cmds: &[
            c("Solidify", ""), c("Fill", ""), c("Recalculate Normals", ""),
            c("Flip Normals", ""), c("Dissolve Faces", ""), c("Delete Faces", ""),
            c("Subdivide", ""), c("Dice Faces", ""),
        ],
    },
    ToolCatDef {
        name: "Select", icon: ph::SELECTION,
        tools: &[
            tw("Box Select", ph::SELECTION), tw("Circle Select", ph::CIRCLE),
            tw("Lasso Select", ph::PATH),
        ],
        cmds: &[
            c("Select All", ""), c("Invert", ""), c("Grow", ""),
            c("Shrink", ""), c("Select Linked", ""), c("Hide Selected", ""),
            c("Hide Unselected", ""), c("Unhide All", ""),
        ],
    },
    ToolCatDef {
        name: "Duplicate", icon: ph::COPY,
        tools: &[
            t("Duplicate", ph::COPY), tw("Mirror", ph::FLIP_HORIZONTAL),
            tw("Array", ph::STACK), tw("Spin/Screw", ph::ARROWS_CLOCKWISE),
        ],
        cmds: &[
            cw("Symmetrize", ""), cw("Make Single User", ""),
        ],
    },
];

pub(crate) const SCULPT_CATS: &[ToolCatDef] = &[
    // ready = true only for brushes that map 1:1 to VoxelBrush / live sculpt path.
    // "soon" left only on palette labels with no engine backend (see activate.rs).
    ToolCatDef {
        name: "Brush", icon: ph::PAINT_BRUSH,
        tools: &[
            t("Draw", ph::PAINT_BRUSH),
            t("Erase", ph::ERASER),
            t("Clay", ph::STAMP),
            t("Clay Buildup", ph::STACK),
            t("Fill", ph::DROP),
            t("Inflate", ph::ARROW_LINE_UP),
            t("Crease", ph::PATH),
            // No VoxelBrush::DrawSharp / Blob / Layer — placeholders stay "soon".
            tw("Draw Sharp", ph::PAINT_BRUSH),
            tw("Blob", ph::SPHERE),
            tw("Layer", ph::STACK),
        ],
        cmds: &[
            c("Reset Brush", ""),
        ],
    },
    ToolCatDef {
        name: "Surface", icon: ph::WAVE_SINE,
        tools: &[
            t("Smooth", ph::WAVE_SINE),
            t("Flatten", ph::ROWS),
            t("Scrape", ph::PENCIL_SIMPLE),
            t("Pinch", ph::ARROWS_IN),
            tw("Multiplane", ph::SQUARE), // no backend
        ],
        cmds: &[
            c("Smooth All", ""),
        ],
    },
    ToolCatDef {
        name: "Grab", icon: ph::HAND_GRABBING,
        tools: &[
            // Grab → VoxelBrush::Move (Grab/Move in engine); Smudge + SnakeHook exist.
            t("Grab", ph::HAND_GRABBING),
            t("Smudge", ph::HAND),
            t("Snake Hook", ph::PATH),
            tw("Elastic", ph::ARROWS_OUT), // no backend
            tw("Nudge", ph::ARROW_LINE_UP),
            tw("Pose", ph::PERSON),
        ],
        cmds: &[
            c("Voxelize", ""),
            c("Remesh", ""),
        ],
    },
    ToolCatDef {
        name: "Mask", icon: ph::SELECTION,
        tools: &[
            // Mask paint is a mode toggle on the active brush path (mask_paint flag).
            t("Mask", ph::SELECTION),
            tw("Box Mask", ph::SQUARE),
            tw("Lasso Mask", ph::PATH),
        ],
        cmds: &[
            c("Invert Mask", ""),
            c("Clear Mask", ""),
            cw("Fill Mask", ""),
            cw("Hide Masked", ""),
        ],
    },
];

pub(crate) const NURBS_CATS: &[ToolCatDef] = &[
    ToolCatDef {
        name: "Surface", icon: ph::SHAPES,
        tools: &[
            tw("Loft", ph::STACK), tw("Revolve", ph::ARROWS_CLOCKWISE),
            tw("Extrude", ph::ARROW_LINE_UP), tw("Sweep", ph::PATH),
            tw("Blend", ph::CIRCLE), tw("Patch", ph::SQUARE),
        ],
        cmds: &[
            cw("Rebuild Tessellation", ""), cw("Flip Normals", ""),
            cw("Close U", ""), cw("Close V", ""),
        ],
    },
    ToolCatDef {
        name: "Edit CV", icon: ph::BEZIER_CURVE,
        tools: &[
            tw("Move CV", ph::ARROWS_OUT_CARDINAL), tw("Weight", ph::DIAMOND),
            tw("Insert Knot U", ph::VECTOR_TWO), tw("Insert Knot V", ph::BEZIER_CURVE),
            tw("Scale CV", ph::CUBE), tw("Box Select", ph::SELECTION),
        ],
        cmds: &[],
    },
    ToolCatDef {
        name: "Convert", icon: ph::ARROWS_LEFT_RIGHT,
        tools: &[],
        cmds: &[
            cw("Convert to Mesh", ""), c("Export OBJ", ""),
            cw("Curve to Mesh", ""), cw("Convert to SDF", ""),
        ],
    },
];

pub(crate) fn tool_categories(p: ToolPillar) -> &'static [ToolCatDef] {
    match p {
        ToolPillar::Sdf => SDF_CATS,
        ToolPillar::Poly => POLY_CATS,
        ToolPillar::Sculpt => SCULPT_CATS,
        ToolPillar::Nurbs => NURBS_CATS,
    }
}

pub(crate) fn pillar_key(p: ToolPillar) -> &'static str {
    match p {
        ToolPillar::Sdf => "sdf",
        ToolPillar::Poly => "poly",
        ToolPillar::Sculpt => "sculpt",
        ToolPillar::Nurbs => "nurbs",
    }
}

pub(crate) fn tool_name_matches(list_name: &str, catalog_key: &str) -> bool {
    list_name == catalog_key
        || (list_name == "Clay Buildup" && catalog_key == "ClayBuildup")
        || (list_name == "Snake Hook" && catalog_key == "SnakeHook")
        || (list_name == "Grab" && (catalog_key == "Grab" || catalog_key == "Move"))
}
