// drawing.rs — Re-exports all drawing modules
// All drawing functions have been extracted to focused draw_*.rs files:
//   draw_gizmo.rs             — gizmo draw/hit, falloff handles, mask gizmo, falloff graph
//   draw_create.rs            — create tool overlay, HUD, dimensions, active tool HUD
//   draw_viewport_overlays.rs — snap indicator, selection overlay, brush preview, toasts,
//                                grid overlay, horizon line, depth pick, billboard camera icon
//   draw_workplane.rs         — workplane overlay
//   draw_selection.rs         — selection highlights, node outlines, SDF brush preview,
//                                raycast select, finish selection drag
//   draw_light.rs             — light gizmos, light hit test
//   draw_transform.rs         — live transform values HUD, smart mirror overlay
//   draw_poly.rs              — face feedback, selected faces, poly overlays, loop cut preview,
//                                bevel/extrude/inset HUD, knife overlay
//   draw_poly_wire.rs         — wireframe overlay, box/circle select overlays, NURBS overlays
