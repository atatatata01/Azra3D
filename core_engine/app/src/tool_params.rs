// core_engine/app/src/tool_params.rs
//
// Gate-2 TOOL PARAMETERS — the data-driven per-tool parameter catalog, ported verbatim
// from the mockup (`docs/UI/azra_app_mockup.html` registerToolParams blocks). Every
// field is a REAL parameter with its real default/range, cited to the Rust source in a
// comment. Rendered with the `ui_fields` kit. UI-ONLY: values ride egui temp memory
// keyed by (tool, group, field) — the next phase swaps that binding for real scene/tool
// state (that is the whole "connect to main code" step, deferred).
//
// Pillar-disambiguated: an SDF Cylinder (radius+height) and a poly Cylinder (sides+caps)
// share a NAME but not a param set. A poly tool is looked up "poly/<Name>" first, falling
// back to the bare name, so SDF primitives keep the bare key and poly shadows them only
// inside the poly pillar — exactly the mockup's rule.
//
// NOT here (owner-corrected, global state, not per-tool): Snapping, Symmetry. Tools with
// no code-level parameters (Bridge, Subdivide, Knife, Merge, Poke) and modifier-only ops
// (Mirror, Array) are simply absent → the panel shows "Aucun paramètre".

use eframe::egui;
use crate::ui_fields as f;
use crate::ui_theme as t;
use crate::DirtyFlags;
use crate::ui_menus::DefaultMenu;

/// One parameter row. Matches the mockup's field vocabulary (num/int/bool/enum/vec3).
pub enum Field {
    Num  { label: &'static str, def: f32, min: Option<f32>, max: Option<f32>, unit: Option<&'static str> },
    Int  { label: &'static str, def: i32, min: i32, max: i32 },
    Bool { label: &'static str, def: bool },
    Enum { label: &'static str, def: &'static str, opts: &'static [&'static str] },
    Vec3 { label: &'static str, def: [f32; 3] },
}

pub struct Group { pub label: &'static str, pub fields: Vec<Field> }
pub struct ToolParams { pub badge: Option<&'static str>, pub groups: Vec<Group> }

// ── field constructors (keep the catalog terse) ─────────────────────────────
fn num(label: &'static str, def: f32, min: Option<f32>, max: Option<f32>) -> Field { Field::Num { label, def, min, max, unit: None } }
fn num_u(label: &'static str, def: f32, min: Option<f32>, max: Option<f32>, unit: &'static str) -> Field { Field::Num { label, def, min, max, unit: Some(unit) } }
fn int(label: &'static str, def: i32, min: i32, max: i32) -> Field { Field::Int { label, def, min, max } }
fn bl(label: &'static str, def: bool) -> Field { Field::Bool { label, def } }
fn en(label: &'static str, def: &'static str, opts: &'static [&'static str]) -> Field { Field::Enum { label, def, opts } }
fn v3(label: &'static str, def: [f32; 3]) -> Field { Field::Vec3 { label, def } }
fn grp(label: &'static str, fields: Vec<Field>) -> Group { Group { label, fields } }

// ── shared groups (mockup consts TRANSFORM / BOOLEAN / CORNERS / BEVEL / SCULPT / …) ──
fn g_transform() -> Group { // primitives.rs:165-167 position/rotation_deg/scale ("extend" = scale)
    grp("Transform", vec![ v3("Offset", [0.0;3]), v3("Rotation", [0.0;3]), v3("Scale", [1.0;3]) ])
}
fn g_boolean() -> Group { // types_create.rs:13 CreateOp; primitives.rs:274 blend default
    grp("Boolean", vec![
        en("Operation", "Union", &["Union","Subtract","Intersect","Slice"]),
        num("Blend K", 0.15, Some(0.0), Some(16.0)),
    ])
}
fn g_corners() -> Group {
    // Full live UI is drawn in draw_box_corners_params (grouped edges + profiles + advanced).
    // Catalog stubs keep section present when unbound.
    grp("Edge Bevels", vec![
        num("Radius (V)", 0.0, Some(0.0), None),
        num("Radius (T/B)", 0.0, Some(0.0), None),
        bl("Chamfer", false),
    ])
}
fn g_bevel() -> Group { // poly_drag_tools.rs:941-945 / bevel_op.rs BevelParams
    grp("Bevel", vec![
        num("Width", 0.0, Some(0.0), None),
        int("Segments", 3, 1, 32),
        num("Shape", 0.5, Some(0.0), Some(1.0)),
        bl("Clamp overlap", true),
    ])
}
fn g_sculpt() -> Vec<Group> {
    // Catalog only — live binding is `draw_tool_params_sculpt` (tool_params_sculpt.rs).
    vec![
        grp("Brush", vec![
            en("Direction", "Add", &["Add", "Subtract"]),
            num("Size", 0.20, Some(0.01), Some(2.0)),
            num("Strength", 0.35, Some(0.0), Some(1.0)),
            en("Falloff", "Smooth", &["Smooth","Smoother","Sphere","Root","Sharp","Linear","InvSq","Constant","Blend"]),
            num("Hardness", 0.0, Some(0.0), Some(1.0)),
        ]),
        grp("Brush Settings", vec![
            bl("Sculpt on Surface", true),
            bl("Accumulate", true),
            bl("Front Faces Only", false),
            bl("Auto Smooth", false),
        ]),
        grp("Texture", vec![
            en("Mapping", "Fixed", &["Fixed", "Follow", "View", "Random"]),
            num("Angle", 0.0, Some(0.0), Some(360.0)),
        ]),
        grp("Stroke", vec![
            en("Method", "Space", &["Dots","Drag Dot","Space","Airbrush","Anchored","Line","Curve"]),
            num("Spacing", 0.10, Some(0.03), Some(0.40)),
            num("Jitter", 0.0, Some(0.0), Some(1.0)),
            num("Stabilize", 0.0, Some(0.0), Some(1.0)),
        ]),
    ]
}
fn g_poly_xf() -> Group { grp("Placement", vec![ v3("Position", [0.0;3]), v3("Scale", [1.0;3]) ]) } // creation.rs:308-314
fn g_xform() -> Vec<Group> { // types_gizmo.rs:13 / types_transform.rs:101 / app_new.rs:180
    vec![
        grp("Orientation", vec![
            en("Repère", "Global", &["Global","Local"]),
            en("Contrainte d'axe", "None", &["None","X","Y","Z","PlaneX","PlaneY","PlaneZ"]),
        ]),
        // Snapping intentionally absent (global — ctxbar Snap pill).
        grp("Édition proportionnelle", vec![
            bl("Activé", false),
            num("Rayon", 1.0, Some(0.05), Some(50.0)),
            en("Falloff", "Smooth", &["Smooth","Sphere","Root","InverseSquare","Sharp","Linear","Constant","Random"]),
        ]),
    ]
}

const LOCAL: Option<&str> = Some("local");
const MESH:  Option<&str> = Some("mesh");

/// The catalog. Returns None when the tool has no parameters (→ "Aucun paramètre").
/// `pillar` disambiguates the shared primitive names ("sdf"/"poly"/"nurbs"/…).
pub fn tool_params(tool: &str, pillar: &str) -> Option<ToolParams> {
    // poly primitives shadow the SDF names, but only in the poly pillar.
    if pillar == "poly" {
        if let Some(tp) = poly_primitive(tool) { return Some(tp); }
    }
    let some = |badge, groups| Some(ToolParams { badge, groups });
    match tool {
        // ── SDF primitive shape tools (creation.rs:17-28, sdf_shapes.rs:106) ──
        "Box"      => some(LOCAL, vec![ grp("Dimensions", vec![v3("Half extents", [0.4;3])]), g_corners(), g_boolean(), g_transform() ]),
        "Sphere"   => some(LOCAL, vec![ grp("Dimensions", vec![num("Radius", 0.4, Some(0.0), None)]), g_boolean(), g_transform() ]),
        "Cylinder" => some(LOCAL, vec![ grp("Dimensions", vec![num("Radius", 0.4, Some(0.0), None), num("Half height", 0.4, Some(0.0), None)]), g_boolean(), g_transform() ]),
        "Cone"     => some(LOCAL, vec![ grp("Dimensions", vec![num("Radius", 0.4, Some(0.0), None), num("Half height", 0.4, Some(0.0), None)]), g_boolean(), g_transform() ]),
        "Torus"    => some(LOCAL, vec![ grp("Dimensions", vec![num("Major radius", 0.3, Some(0.0), None), num("Minor radius", 0.1, Some(0.001), None)]), g_boolean(), g_transform() ]), // minor clamp 0.001..major creation.rs:468
        "Capsule"  => some(LOCAL, vec![ grp("Dimensions", vec![num("Radius", 0.2, Some(0.0), None), num("Half height", 0.2, Some(0.0), None)]), g_boolean(), g_transform() ]),
        "Plane" | "Slice" => some(LOCAL, vec![ g_boolean(), g_transform() ]), // infinite half-space, no shape params (sdf_shapes.rs:152,359)

        // ── boolean tools (primitives.rs:45 BooleanOp) ──
        "Union"     => some(None, vec![ grp("Blend", vec![num("Blend K", 0.15, Some(0.0), Some(16.0))]) ]),
        "Subtract"  => some(None, vec![ grp("Blend", vec![num("Blend K", 0.0,  Some(0.0), Some(16.0))]) ]),
        "Intersect" => some(None, vec![ grp("Blend", vec![num("Blend K", 0.0,  Some(0.0), Some(16.0))]) ]),
        "Blend K"   => some(None, vec![ grp("Blend", vec![num("Blend K", 0.15, Some(0.0), Some(16.0))]) ]),
        "Elongate"  => some(None, vec![ grp("Elongation", vec![v3("Amount", [0.0;3])]) ]), // primitives.rs:286

        // ── poly interactive tools (types_poly.rs / bevel_op.rs / poly_drag_tools.rs) ──
        "Bevel"            => some(Some("edge"),   vec![ g_bevel() ]),
        "Bevel Vertices"   => some(Some("vertex"), vec![ g_bevel() ]),
        "Extrude" | "Extrude Depth" => some(None, vec![ grp("Extrude", vec![num("Depth", 0.0, None, None)]) ]), // signed :107
        "Inset"            => some(None, vec![ grp("Inset", vec![num("Thickness", 0.0, None, None), num("Depth", 0.0, None, None), bl("Individual", false)]) ]),
        "Shrink/Fatten"    => some(None, vec![ grp("Offset", vec![num("Offset", 0.0, None, None)]) ]),
        "Loop Cut"         => some(None, vec![ grp("Loop Cut", vec![ int("Cuts", 1, 1, 20), num("Slide", 0.0, Some(-1.0), Some(1.0)), bl("Clamp", true), bl("Paired", false), bl("Live", false), bl("Selected", true) ]) ]),
        "Edge Slide"       => some(None, vec![ grp("Slide", vec![num("Factor", 0.0, Some(-1.0), Some(1.0)), bl("Clamp", true)]) ]),
        "Vertex Slide"     => some(None, vec![ grp("Slide", vec![num("Factor", 0.0, Some(0.0), Some(1.0)), bl("Clamp", true)]) ]),
        "Solidify"         => some(None, vec![ grp("Solidify", vec![num("Thickness", 0.1, Some(0.0), None)]) ]),
        "Merge by Distance"=> some(Some("vertex"), vec![ grp("Merge", vec![num_u("Threshold", 0.001, Some(0.0001), None, "m")]) ]), // merge.rs:74
        // Live params are drawn by `draw_tool_params_mesh_cleanup` (bound to ui.mesh_cleanup_*).
        "Mesh Cleanup"     => some(Some("mesh"), vec![ grp("Cleanup", vec![
            bl("Flatten folded flaps", true),
            bl("Merge coplanar faces", true),
            num("Limited dissolve °", 4.5, Some(0.0), Some(15.0)),
        ]) ]),

        // ── selection tools (types_poly.rs region/circle select) ──
        "Box Select" | "Lasso Select" => some(None, vec![ grp("Selection", vec![bl("Additive", false), bl("Sélection au travers", false)]) ]),
        "Circle Select" => some(None, vec![ grp("Selection", vec![num_u("Rayon", 40.0, Some(1.0), None, "px"), bl("Additive", false), bl("Sélection au travers", false)]) ]),

        // ── transform / gizmo tools (Snapping is global, not here) ──
        "Move" | "Rotate" | "Scale" => some(None, g_xform()),

        // ── voxel sculpt brushes share one param set. "Move" is deliberately absent:
        //    the transform gizmo "Move" (above) wins the shared name — the mockup's
        //    documented decision (Object mode is the default). ──
        "Draw" | "Erase" | "Smooth" | "Flatten" | "Clay" | "Scrape" | "Crease" | "Fill"
        | "ClayBuildup" | "Inflate" | "Pinch" | "Smudge" => some(None, g_sculpt()),

        _ => None,
    }
}

/// Catalog entry for a poly primitive — the no-app-binding fallback (demos, empty selection).
///
/// DERIVED from `PrimitiveSpec` rather than hand-listed: this used to be a third copy of the
/// parameter list, silently free to disagree with the real sliders. It now only has to know which
/// tool name maps to which spec; the parameters themselves come from the spec's defaults.
fn poly_primitive(tool: &str) -> Option<ToolParams> {
    use core_math::primitive_spec::PrimitiveSpec as P;
    let kind = match tool {
        "Cube" => "box",
        "Plane" => "plane",
        "Circle" => "circle",
        "Sphere" => "sphere",
        "Ico Sphere" => "icosphere",
        "Cylinder" => "cylinder",
        "Cone" => "cone",
        "Torus" => "torus",
        _ => return None,
    };
    let spec = P::from_kind_id(kind)?;
    let max_sides = core_math::primitive_spec::MAX_SIDES as i32;
    let max_sub = core_math::primitive_spec::MAX_SUBDIV as i32;
    let geom = match spec {
        P::Box(s) => vec![
            num("Size X", s.size.x, Some(0.01), Some(20.0)),
            num("Size Y", s.size.y, Some(0.01), Some(20.0)),
            num("Size Z", s.size.z, Some(0.01), Some(20.0)),
            int("Segments X", s.seg_x as i32, 1, max_sub),
            int("Segments Y", s.seg_y as i32, 1, max_sub),
            int("Segments Z", s.seg_z as i32, 1, max_sub),
            num("Round", s.round.radius, Some(0.0), Some(2.0)),
        ],
        P::Plane(s) => vec![
            num("Width", s.width, Some(0.01), Some(40.0)),
            num("Depth", s.depth, Some(0.01), Some(40.0)),
            int("Subdiv W", s.subdivs_w as i32, 1, max_sub),
            int("Subdiv D", s.subdivs_d as i32, 1, max_sub),
        ],
        P::Circle(s) => vec![
            num("Radius", s.radius, Some(0.01), Some(20.0)),
            int("Sides", s.sides as i32, 3, max_sides),
            num("Sweep", s.arc.sweep_deg, Some(1.0), Some(360.0)),
        ],
        P::UvSphere(s) => vec![
            num("Radius", s.radius, Some(0.01), Some(20.0)),
            int("Segments", s.segments as i32, 3, max_sides),
            int("Rings", s.rings as i32, 2, max_sub),
            num("Sweep", s.arc.sweep_deg, Some(1.0), Some(360.0)),
        ],
        P::IcoSphere(s) => vec![
            num("Radius", s.radius, Some(0.01), Some(20.0)),
            int(
                "Subdivisions",
                s.subdivisions as i32,
                0,
                core_math::primitive_spec::MAX_ICO_SUBDIV as i32,
            ),
        ],
        P::Cylinder(s) => vec![
            num("Radius", s.radius, Some(0.01), Some(20.0)),
            num("Height", s.height, Some(0.01), Some(40.0)),
            int("Sides", s.sides as i32, 3, max_sides),
            int("V Segments", s.vsegments as i32, 1, max_sub),
            num("Sweep", s.arc.sweep_deg, Some(1.0), Some(360.0)),
            num("Round", s.round.radius, Some(0.0), Some(2.0)),
        ],
        P::Cone(s) => vec![
            num("Radius Bottom", s.radius_bottom, Some(0.01), Some(20.0)),
            num("Radius Top", s.radius_top, Some(0.0), Some(20.0)),
            num("Height", s.height, Some(0.01), Some(40.0)),
            int("Sides", s.sides as i32, 3, max_sides),
            int("V Segments", s.vsegments as i32, 1, max_sub),
            num("Sweep", s.arc.sweep_deg, Some(1.0), Some(360.0)),
        ],
        P::Torus(s) => vec![
            num("Radius", s.major_r, Some(0.01), Some(20.0)),
            num("Minor Radius", s.minor_r, Some(0.01), Some(20.0)),
            int("Major Segments", s.major_segs as i32, 3, max_sides),
            int("Minor Segments", s.minor_segs as i32, 3, max_sides),
            num("Sweep", s.arc.sweep_deg, Some(1.0), Some(360.0)),
        ],
    };
    Some(ToolParams {
        badge: MESH,
        groups: vec![grp("Geometry", geom), g_poly_xf()],
    })
}

/// Catalog-only draw (temp memory) — used by demos / when no app binding.
pub fn draw(ui: &mut egui::Ui, tool: &str, pillar: &str) {
    let tp = match tool_params(tool, pillar) {
        Some(tp) => tp,
        None => {
            empty_msg(ui, tool);
            return;
        }
    };
    for g in &tp.groups {
        let sid = format!("{tool}/{}", g.label);
        f::section(ui, &sid, g.label, tp.badge, |ui| {
            for field in &g.fields {
                draw_field_temp(ui, tool, g.label, field);
            }
        });
    }
}

fn empty_msg(ui: &mut egui::Ui, tool: &str) {
    let msg = if tool.is_empty() {
        "No active tool.".to_owned()
    } else {
        format!("{tool} — no parameters.")
    };
    ui.add_space(12.0);
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new(msg).size(11.0).color(t::TEXT_FAINT))
    });
}

fn draw_field_temp(ui: &mut egui::Ui, tool: &str, group: &str, field: &Field) {
    match field {
        Field::Num { label, def, min, max, unit } => {
            let id = fid(tool, group, label);
            let mut v = ui.data_mut(|d| d.get_temp::<f32>(id).unwrap_or(*def));
            let range = min.unwrap_or(f32::MIN)..=max.unwrap_or(f32::MAX);
            f::num(ui, label, &mut v, 0.01, range, *unit);
            ui.data_mut(|d| d.insert_temp(id, v));
        }
        Field::Int { label, def, min, max } => {
            let id = fid(tool, group, label);
            let mut v = ui.data_mut(|d| d.get_temp::<i32>(id).unwrap_or(*def));
            f::int(ui, label, &mut v, *min..=*max);
            ui.data_mut(|d| d.insert_temp(id, v));
        }
        Field::Bool { label, def } => {
            let id = fid(tool, group, label);
            let mut v = ui.data_mut(|d| d.get_temp::<bool>(id).unwrap_or(*def));
            f::bool_row(ui, label, &mut v);
            ui.data_mut(|d| d.insert_temp(id, v));
        }
        Field::Enum { label, def, opts } => {
            let id = fid(tool, group, label);
            let mut v = ui
                .data_mut(|d| d.get_temp::<String>(id).unwrap_or_else(|| (*def).to_owned()));
            if opts.len() <= 3 {
                let pairs: Vec<(&str, String)> =
                    opts.iter().map(|o| (*o, (*o).to_owned())).collect();
                f::enum_seg(ui, label, &mut v, &pairs);
            } else {
                f::enum_combo(ui, label, &mut v, opts);
            }
            ui.data_mut(|d| d.insert_temp(id, v));
        }
        Field::Vec3 { label, def } => {
            let id = fid(tool, group, label);
            let mut v = ui.data_mut(|d| d.get_temp::<[f32; 3]>(id).unwrap_or(*def));
            f::vec3_row(ui, label, &mut v);
            ui.data_mut(|d| d.insert_temp(id, v));
        }
    }
}

fn fid(tool: &str, group: &str, label: &str) -> egui::Id {
    egui::Id::new(("tool_param", tool, group, label))
}

/// Map an SDF shape to the tool-params catalog key.
pub fn tool_name_from_shape(shape: &core_math::primitives::SdfShape) -> &'static str {
    use core_math::primitives::SdfShape;
    match shape {
        SdfShape::Box { .. } => "Box",
        SdfShape::Sphere { .. } => "Sphere",
        SdfShape::Cylinder { .. } => "Cylinder",
        SdfShape::Cone { .. } => "Cone",
        SdfShape::Torus { .. } => "Torus",
        SdfShape::Capsule { .. } => "Capsule",
        SdfShape::HalfSpace => "Plane",
        SdfShape::Stair { .. } => "Box",
        _ => "Box",
    }
}

fn op_label(op: &core_math::primitives::BooleanOp) -> &'static str {
    use core_math::primitives::BooleanOp;
    match op {
        BooleanOp::Union { .. } | BooleanOp::ChamferUnion { .. } => "Union",
        BooleanOp::Subtract { .. } | BooleanOp::ChamferSubtract { .. } => "Subtract",
        BooleanOp::Intersect | BooleanOp::SmoothIntersect { .. } => "Intersect",
        BooleanOp::Seam { .. } | BooleanOp::Extrude { .. } => "Slice",
    }
}

fn set_op_kind(op: &mut core_math::primitives::BooleanOp, label: &str) {
    use core_math::primitives::BooleanOp;
    let k = op.blend_k();
    *op = match label {
        "Subtract" => BooleanOp::Subtract { k },
        "Intersect" => {
            if k > 0.0 {
                BooleanOp::SmoothIntersect { k }
            } else {
                BooleanOp::Intersect
            }
        }
        "Slice" => BooleanOp::Seam {
            width: k.max(0.05),
            depth: 0.02,
            profile: 0,
            raised: false,
        },
        _ => BooleanOp::Union { k },
    };
}

impl crate::FourthLayerApp {
    /// Live parameters for the KNIFE (owner 2026-08-04: "I should see all the knife
    /// parameters"). Angle step, Enter/RMB free-line finish mode (subtract vs scribe),
    /// cut shapes, face-restriction state.
    ///
    /// **S is NOT a mode toggle.** Pressing S applies a through-slice immediately
    /// (one-shot verb). Owner 2026-08-06: the old checkbox claimed "key: S" and read like a
    /// toggle — wrong; S just makes a slice.
    pub(crate) fn draw_tool_params_poly_knife(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new("Live · Knife")
                .size(11.0)
                .color(egui::Color32::from_rgb(160, 170, 190)),
        );
        ui.add_space(2.0);
        let pts = self.knife_points.len();
        ui.label(
            egui::RichText::new(if self.knife_free { "Free polyline" } else { "Surface cut" })
                .size(10.0)
                .color(egui::Color32::from_rgb(140, 148, 165)),
        );
        let r = ui.add(
            egui::Slider::new(&mut self.viewport.snap_rotation_deg, 0.0..=90.0)
                .text("Angle step °"),
        );
        if r.changed() {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        // Enter / RMB finish mode for free polyline only — NOT bound to S.
        let mut slice = self.knife_free_slice;
        if ui
            .checkbox(&mut slice, "On confirm: scribe only")
            .on_hover_text(
                "Free polyline · Enter / RMB only:\n\
                 • Off = SUBTRACT — remove material on the side of the line\n\
                 • On  = SCRIBE — insert cut edges, keep all material\n\n\
                 S is separate: press S to APPLY a through-slice right now (not a toggle).",
            )
            .changed()
        {
            self.knife_free_slice = slice;
        }
        ui.label(
            egui::RichText::new("S = apply through-slice now")
                .size(10.0)
                .color(egui::Color32::from_rgb(140, 148, 165)),
        )
        .on_hover_text(
            "One-shot verb while the knife has ≥2 points: cuts through immediately.\n\
             Does not flip the checkbox above.",
        );
        if !self.poly_sel.selected_faces.is_empty() {
            ui.label(
                egui::RichText::new(format!(
                    "Restricted to {} selected face(s)",
                    self.poly_sel.selected_faces.len()
                ))
                .size(10.0)
                .color(egui::Color32::from_rgb(255, 190, 80)),
            );
        }
        ui.separator();
        // CUT SHAPE — also R (rectangle) / C (circle) while Knife is active (state-gated so
        // they do not steal Rotate / Circle Select outside the knife modal).
        ui.label(
            egui::RichText::new("Cut shape  (R rect · C circle · L free line)")
                .size(10.0)
                .color(egui::Color32::from_rgb(140, 148, 165)),
        );
        let mut kind = self.knife_shape_kind;
        ui.horizontal(|ui| {
            for (k, label, tip) in [
                (0u8, "Free line", "L — click to place points one by one."),
                (
                    1u8,
                    "Rectangle",
                    "R — drag corner→corner. Alt = from center.\n\
                     Release = hole (subtract). S = slice.\n\
                     Faces selected → always slice those faces only.",
                ),
                (
                    2u8,
                    "Circle",
                    "C — drag radius from center. Alt = from center.\n\
                     Release = hole (subtract). S = slice.\n\
                     Faces selected → always slice those faces only.",
                ),
            ] {
                if ui
                    .selectable_label(kind == k, label)
                    .on_hover_text(tip)
                    .clicked()
                {
                    kind = k;
                }
            }
        });
        ui.label(
            egui::RichText::new(
                "Alt = from center · S = slice all islands · Face mode + face sel = those polys only",
            )
            .size(9.5)
            .color(egui::Color32::from_rgb(140, 148, 165)),
        );
        if kind != self.knife_shape_kind {
            if kind == 0 {
                self.poly_knife_shape_to_free_line();
            } else {
                self.poly_knife_shape_arm(kind);
            }
        }
        // BOOLEAN TYPE of the shape cut (owner 2026-08-11) — the same three ops the U / I chords
        // pick, so the keys and the panel are one state, not two.
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Boolean  (U union · I intersect · again = subtract)")
                .size(10.0)
                .color(egui::Color32::from_rgb(140, 148, 165)),
        );
        ui.horizontal(|ui| {
            use crate::poly_knife::KnifeBoolOp;
            for (op, label, tip) in [
                (
                    KnifeBoolOp::Subtract,
                    "Subtract",
                    "The shape REMOVES material (cookie cutter).\n\
                     E = pocket depth (pull INTO the surface); no depth = through hole.",
                ),
                (
                    KnifeBoolOp::Union,
                    "Union",
                    "The shape ADDS material — a boss/pad standing on the surface.\n\
                     Needs a height: press E and pull OUT of the surface.\n\
                     The base is sunk slightly into the solid so the join is never a\n\
                     coplanar face pair (which is what makes booleans fold).",
                ),
                (
                    KnifeBoolOp::Intersect,
                    "Intersect",
                    "KEEPS only the material the outline encloses, discards the rest.\n\
                     Closed outline + closed mesh only.",
                ),
            ] {
                if ui
                    .selectable_label(self.knife_bool_op == op, label)
                    .on_hover_text(tip)
                    .clicked()
                    && self.knife_bool_op != op
                {
                    // Route through the same setter the keys use (it toasts + marks dirty);
                    // it toggles on re-press, so only call it when the op actually changes.
                    self.poly_knife_set_bool_op(op);
                }
            }
        });
        // Bevel segment count — default 3. Rect: Shift+scroll while dragging; Circle: scroll.
        {
            let mut seg = self.knife_shape_segments as i32;
            if ui
                .add(egui::Slider::new(&mut seg, 3..=64).text("Bevel segments"))
                .on_hover_text(
                    "Arc samples per rect corner (default 3).\n\
                     Rectangle drag: Shift+scroll changes this (one step per wheel tick).\n\
                     Circle drag: plain scroll changes this.\n\
                     Also editable here anytime while mid-drag.",
                )
                .changed()
            {
                self.knife_shape_segments = seg.clamp(3, 64) as u32;
                self.poly_knife_shape_refresh();
            }
        }
        // Corner bevel amount — tool setting + plain scroll while rectangle mid-drag.
        {
            let mut b = self.knife_shape_corner_bevel;
            if ui
                .add(egui::Slider::new(&mut b, 0.0..=1.0).text("Corner bevel"))
                .on_hover_text(
                    "Rectangle corner roundness (0 = sharp, 1 = max fillet).\n\
                     Plain scroll while dragging the rectangle adjusts this.\n\
                     Shift+scroll = bevel segment count instead.",
                )
                .changed()
            {
                self.knife_shape_corner_bevel = b.clamp(0.0, 1.0);
                self.poly_knife_shape_refresh();
            }
        }
        let mut sel_after = self.knife_select_after_cut;
        if ui
            .checkbox(&mut sel_after, "Select after cut")
            .on_hover_text(
                "On by default: a cut hands back what it produced.\n\
                 Boolean cut (subtract / Alt+S slice) → the new cut faces (Face mode).\n\
                 Scribe / surface cut → the new rim edges (Edge mode).\n\
                 Off: stay in Knife ready for another cut without selection jumping.",
            )
            .changed()
        {
            self.knife_select_after_cut = sel_after;
        }
        ui.separator();
        // PIPE target, set BEFORE pressing P (owner: own modifier stack vs poly island).
        let mut own = self.tweak.pipe_own_object;
        if ui
            .checkbox(&mut own, "Pipe (P) makes its own object")
            .on_hover_text(
                "On: P builds a new object with its own modifier stack.
                 Off: P adds the tube as an island inside the active mesh.",
            )
            .changed()
        {
            self.tweak.pipe_own_object = own;
        }
        if pts >= 2 {
            ui.label(
                egui::RichText::new("P = turn this polyline into a pipe")
                    .size(10.0)
                    .color(egui::Color32::from_rgb(255, 190, 80)),
            );
        }
        ui.separator();
        ui.label(
            egui::RichText::new(if pts >= 3 {
                "LMB place · double-click the start to close · Enter cut · Esc cancel"
            } else {
                "LMB place · Ctrl H/V · Shift/magnet angle · Enter cut · Esc cancel"
            })
            .size(10.0)
            .color(egui::Color32::from_rgb(140, 148, 165)),
        );
        if pts > 0 {
            ui.label(
                egui::RichText::new(format!("{pts} point(s) placed"))
                    .size(10.0)
                    .color(egui::Color32::from_rgb(140, 148, 165)),
            );
        }
    }

    /// Live Mesh Cleanup parameters (left Tool Parameters panel).
    /// Bound to `ui.mesh_cleanup_*` — same knobs as Mesh > Cleanup dialog.
    /// Ctrl+X (edit mode, empty selection) arms this tool at 4.5° limited dissolve.
    pub(crate) fn draw_tool_params_mesh_cleanup(&mut self, ui: &mut egui::Ui) {
        // Ensure a live session while this panel is open.
        if let Some(nid) = self.poly_cleanup_target() {
            self.mesh_cleanup_begin_session(nid);
        }
        ui.label(
            egui::RichText::new(if self.ui.mesh_cleanup_live {
                "Live · Mesh Cleanup"
            } else {
                "Mesh Cleanup"
            })
            .size(11.0)
            .color(egui::Color32::from_rgb(160, 170, 190)),
        );
        ui.label(
            egui::RichText::new(
                "Live ON (default): change settings and the mesh updates immediately. \
                 Apply commits (undoable). Ctrl+X empty-edit runs at 4.5° then select-all.",
            )
            .size(10.0)
            .color(egui::Color32::from_rgb(140, 148, 165)),
        );
        ui.add_space(4.0);

        // ONE widget block, shared with the Mesh > Cleanup dialog (`draw_mesh_cleanup_controls`),
        // so the ten knobs cannot drift between the two homes.
        let knobs_changed = self.draw_mesh_cleanup_controls(ui);
        if knobs_changed && self.ui.mesh_cleanup_live {
            self.mesh_cleanup_live_regenerate();
        }
        ui.weak("Tagged / sharp / seam edges are never removed.");

        ui.add_space(6.0);
        let mut apply = false;
        let mut reapply_select = false;
        let mut cancel_live = false;
        ui.horizontal(|ui| {
            if ui
                .button(if self.ui.mesh_cleanup_live {
                    "Apply (commit)"
                } else {
                    "Apply"
                })
                .on_hover_text(
                    "Commit cleanup with the parameters above (undoable). Live preview is not \
                     on the undo stack until you Apply.",
                )
                .clicked()
            {
                apply = true;
            }
            if ui
                .button("Apply + Select All")
                .on_hover_text("Same as Ctrl+X empty-edit: cleanup, then select all.")
                .clicked()
            {
                reapply_select = true;
            }
            if self.mesh_cleanup_base.is_some()
                && ui
                    .button("Cancel")
                    .on_hover_text("Discard live preview and restore the mesh.")
                    .clicked()
            {
                cancel_live = true;
            }
        });

        if let Some(rep) = self.ui.mesh_cleanup_result.as_ref() {
            ui.separator();
            ui.label(
                egui::RichText::new(if rep.applied {
                    "Last run"
                } else {
                    "Last run — no change"
                })
                .size(10.5)
                .color(egui::Color32::from_rgb(160, 170, 190)),
            );
            if rep.applied {
                ui.monospace(format!(
                    "Verts  Δ{}  ({} → {})",
                    rep.verts_merged(),
                    rep.verts_before,
                    rep.verts_after
                ));
                ui.monospace(format!(
                    "Edges  Δ{}  ({} → {})",
                    rep.edges_dissolved(),
                    rep.edges_before,
                    rep.edges_after
                ));
                ui.monospace(format!(
                    "Faces  Δ{}  ({} → {})",
                    rep.faces_removed(),
                    rep.faces_before,
                    rep.faces_after
                ));
                ui.monospace(format!(
                    "Folds  Δ{}  ({} → {})",
                    rep.folds_fixed(),
                    rep.folds_before,
                    rep.folds_after
                ));
                let s = rep.stats;
                if s.verts_welded > 0 {
                    ui.monospace(format!("Welded  {}", s.verts_welded));
                }
                if s.degenerate_removed > 0 {
                    ui.monospace(format!("Degen   {}", s.degenerate_removed));
                }
                if !s.loose.is_empty() {
                    ui.monospace(format!(
                        "Loose   {}v {}e {}f",
                        s.loose.verts, s.loose.edges, s.loose.faces
                    ));
                }
                if s.holes_filled > 0 {
                    ui.monospace(format!("Holes   {}", s.holes_filled));
                }
                if s.planar_faces_fixed > 0 {
                    ui.monospace(format!("Planar  {}", s.planar_faces_fixed));
                }
                if s.non_planar_split + s.concave_split > 0 {
                    ui.monospace(format!(
                        "Split   +{} face(s)",
                        s.non_planar_split + s.concave_split
                    ));
                }
                if s.normals_flipped > 0 {
                    ui.monospace(format!("Rewound {} face(s)", s.normals_flipped));
                }
            } else {
                ui.weak("Mesh was already clean.");
            }
        }

        if cancel_live {
            self.mesh_cleanup_end_session(true);
            self.ui.mesh_cleanup_result = None;
            self.ui2.active_tool.clear();
        }
        if apply || reapply_select {
            if let Some(nid) = self.poly_cleanup_target() {
                self.poly_mesh_cleanup_now_ex(nid, reapply_select);
            } else {
                self.toast("Cleanup: select a polygonal body first");
            }
        }
    }

    /// **Single home** for tool parameters (Live · Last · Selection · Active).
    ///
    /// Priority (owner one-home rule — no floating Tool Properties / Adjust windows in ui2):
    /// 1. **Live** modal tool (`tool.current`)
    /// 2. **Armed** non-modal (pipe / knife / mesh cleanup)
    /// 3. **Sculpt** when `WorkMode::Sculpt` (voxel or Surface poly)
    /// 4. **Armed** UV Unwrap / Mesh Maps (`ui2.active_tool` only — not "any selected poly")
    /// 5. **Last** adjust (bevel / inset / circle / tosphere / create / NURBS fillet)
    /// 6. **Selection** — selected SDF prim → live scene binding
    /// 7. **Active** — palette `ui2.active_tool` catalog defaults
    pub(crate) fn ui2_draw_tool_params(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = 2.0;
        // Blender F9 rule: any later edit = auto-accept last-create (no sticky Done).
        self.poly_create_adjust_auto_accept_if_stale();

        // ── 1) Live modal tools ─────────────────────────────────────────────
        if self.tool.current.is_poly_bevel() {
            self.draw_tool_params_poly_bevel(ui);
            return;
        }
        if self.tool.current.is_poly_extrude() || self.tool.current.is_poly_edge_extrude() {
            self.draw_tool_params_poly_extrude(ui);
            return;
        }
        if self.tool.current.is_poly_inset() {
            self.draw_tool_params_poly_inset(ui);
            return;
        }
        // Armed OR mid-slide — see `draw_tool_params_poly_loop_cut`: gating on the session alone
        // hid the whole panel whenever Ctrl+R was pressed without an edge under the cursor.
        if self.poly_loop_cut_is_active() {
            self.draw_tool_params_poly_loop_cut(ui);
            return;
        }
        // Edge mode idle: MESHmachine-style Loop Select panel (Alt+click / Alt+Ctrl ring).
        if self.modeling_context == crate::ModelingContext::Polygonal
            && self.poly_work_mode == crate::PolyWorkMode::Edge
            && self.poly_tool == crate::PolyTool::Idle
            && self.tool.current.is_idle()
        {
            self.draw_tool_params_loop_select(ui);
            return;
        }
        if self.tool.current.is_poly_shrink_fatten() {
            self.draw_tool_params_poly_shrink_fatten(ui);
            return;
        }
        if self.tool.current.is_poly_edge_weight() {
            self.draw_tool_params_poly_edge_weight(ui);
            return;
        }
        if self.tool.current.is_poly_slide() {
            self.draw_tool_params_poly_slide(ui);
            return;
        }
        if self.tool.current.is_poly_transform() {
            self.draw_tool_params_poly_transform(ui);
            return;
        }
        if self.tool.current.is_nurbs_fillet() {
            self.draw_tool_params_nurbs_fillet_live(ui);
            return;
        }
        if self.tool.current.is_create() {
            self.draw_tool_params_create_live(ui);
            return;
        }
        if self.tool.current.is_transform() {
            self.draw_tool_params_object_transform(ui);
            // Nested sub-body G/R/S: also show Blend K / boolean (like prim live params).
            if let Some(nid) = self.active_nested_sdf_body().or_else(|| {
                // Transform may target a nested body via node_target even if selection drifted.
                self.tool
                    .current
                    .as_transform()
                    .and_then(|ts| ts.node_target)
                    .filter(|&nid| self.is_nested_sdf_body(nid))
            }) {
                ui.add_space(6.0);
                ui.separator();
                let _ = self.draw_subbody_boolean_params(ui, nid);
            }
            return;
        }

        // ── 1b) Live NON-MODAL poly tool (armed from the palette / a key) ──────
        // Owner 2026-08-04: "the tool parameter needs to stay active until the user chooses
        // another tool — here it stays on Extrude but I use Knife, so I should see all the
        // knife parameters." A tool the user just picked outranks ANY last-operation redo
        // panel below: the redo panel is for the op you finished, not the tool you armed.
        if self.pipe_adjust_active() {
            self.draw_tool_params_poly_pipe(ui);
            return;
        }
        if self.poly_tool == crate::PolyTool::Knife {
            self.draw_tool_params_poly_knife(ui);
            return;
        }
        // Mesh Cleanup (Ctrl+X empty edit / Mesh menu) — live params in left panel.
        if self.ui2.active_tool == "Mesh Cleanup" {
            self.draw_tool_params_mesh_cleanup(ui);
            return;
        }

        // ── 3) Sculpt brush — Surface or Voxel. Outranks last-adjust / UV / Maps. ──
        if self.tool.work_mode == crate::WorkMode::Sculpt
            && (self.sculpt.voxel.is_some() || self.sculpt.poly_edit.is_some())
        {
            self.draw_tool_params_sculpt(ui);
            return;
        }

        // UV Unwrap / Mesh Maps only when that tool is armed — never because a poly is selected.
        if self.uv_unwrap_adjust_active() {
            self.draw_tool_params_uv_unwrap(ui);
            return;
        }
        if self.tool_params_mesh_maps_active() {
            self.draw_tool_params_mesh_maps(ui);
            return;
        }

        // ── 2) Last-operation adjust (was floating; now dock-only in ui2) ───
        if self.poly_bevel_adjust_active() {
            let _ = self.poly_bevel_adjust_panel(ui);
            return;
        }
        if self.poly_inset_adjust_active() {
            let _ = self.poly_inset_adjust_panel(ui);
            return;
        }
        if self.poly_extrude_adjust_active() {
            let _ = self.poly_extrude_adjust_panel(ui);
            return;
        }
        if self.poly_circle_adjust_active() {
            let _ = self.poly_circle_adjust_panel(ui);
            return;
        }
        if self.poly_tosphere_adjust_active() {
            let _ = self.poly_tosphere_adjust_panel(ui);
            return;
        }
        if self.poly_boolean_adjust_active() {
            let _ = self.poly_boolean_adjust_panel(ui);
            return;
        }
        if self.ui.poly_create_adjust.is_some() {
            self.draw_tool_params_poly_create_adjust(ui);
            return;
        }
        if self.nurbs_fillet_adjust_active() {
            let _ = self.nurbs_fillet_adjust_panel(ui);
            return;
        }

        // ── 3b) Mesh Terrain tile / island — tile settings + add/stream/export ─
        // Shift+A only adds Terrain; all other terrain ops live here (owner).
        if self.draw_tool_params_terrain(ui) {
            return;
        }

        // ── 4) Selected SDF prim → live scene ──────────────────────────────
        if let Some(idx) = self.active_prim_index_unified() {
            if idx < self.scene.primitives.len() {
                self.draw_tool_params_bound(ui, idx);
                return;
            }
        }

        // ── 4a) Nested SDF sub-body → Boolean + Blend K (own tool params) ───
        // Owner: sub-bodies need the same blend-K authoring as primitives — not only free TRS.
        if let Some(nid) = self.active_nested_sdf_body() {
            let _ = self.draw_subbody_boolean_params(ui, nid);
            return;
        }

        // ── 4b) Selected poly parametric body → live Sides / Radius / … ─────
        // Owner: "cylinder segment do not work parametric" — selecting a mesh cylinder
        // fell through to Active · catalog defaults (temp memory only), so Sides never
        // rebuilt the mesh. Same widget as Properties + last-create adjust.
        if self.draw_tool_params_poly_parametric_selected(ui) {
            return;
        }

        // ── 5) Active / palette catalog defaults ───────────────────────────
        let tool = if self.ui2.active_tool.is_empty() {
            "Box".to_owned()
        } else {
            self.ui2.active_tool.clone()
        };
        let pillar = if self.ui2.active_pillar.is_empty() {
            "sdf".to_owned()
        } else {
            self.ui2.active_pillar.clone()
        };
        ui.label(
            egui::RichText::new(format!("Active · {tool}"))
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        ui.label(
            egui::RichText::new("Defaults for next create / tool arm (select a prim or run a tool for live params)")
                .size(10.0)
                .color(t::TEXT_FAINT),
        );
        ui.add_space(2.0);
        draw(ui, &tool, &pillar);
    }

    /// Live parametric poly body (selected node still matches its PrimitiveSpec).
    /// Returns true when this panel owned the draw (caller should not fall through).
    fn draw_tool_params_poly_parametric_selected(&mut self, ui: &mut egui::Ui) -> bool {
        use core_math::scene_graph::NodePayload;
        use crate::SceneItemId;
        let nid = match self.sel.usel.active_node().or_else(|| {
            match self.sel.usel.last_clicked {
                Some(SceneItemId::Node(n)) => Some(n),
                _ => None,
            }
        }) {
            Some(n) => n,
            None => return false,
        };
        // Confirm it's a poly body before claiming the panel.
        let body_name = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { name, .. }) => name.clone(),
            _ => return false,
        };

        let Some(mut spec) = self.poly_live_spec(nid) else {
            if self.poly_spec_went_stale(nid) {
                ui.label(
                    egui::RichText::new(format!("Selected · {body_name}"))
                        .size(11.0)
                        .color(t::TEXT_SECONDARY),
                );
                ui.label(
                    egui::RichText::new(
                        "Edited — no longer parametric. Make a new primitive or undo topology edits \
                         to restore Sides / Radius controls.",
                    )
                    .size(10.0)
                    .color(t::TEXT_FAINT),
                );
                return true;
            }
            // Authored mesh without a live spec — don't steal catalog defaults.
            return false;
        };

        let kind = spec.label();
        ui.label(
            egui::RichText::new(format!("Selected · {kind}"))
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        ui.label(
            egui::RichText::new("Live parametric — changing Sides rebuilds the mesh")
                .size(10.0)
                .color(t::TEXT_FAINT),
        );
        ui.add_space(2.0);

        f::section(ui, "live/PolyParam/geom", "Geometry", Some("mesh"), |ui| {
            let e = crate::poly::primitive_params_ui::draw_primitive_spec_params(ui, &mut spec);
            if e.changed {
                if let Some(sides) = crate::creation::spec_sides_to_remember(&spec) {
                    self.ui.poly_create_last_sides = sides;
                }
                self.set_primitive_spec(nid, spec);
            }
            if e.released {
                self.poly_request_display_sync(nid);
                self.transaction_snapshot_only(&format!("Adjust {kind}"));
            }
        });

        // Placement (body transform) — same fields as last-create adjust.
        if let Some(node) = self.scene.scene_graph.get(nid) {
            let mut pos = node.local_position;
            let mut scale = node.local_scale;
            let mut xform_changed = false;
            let mut released = false;
            f::section(ui, "live/PolyParam/place", "Placement", Some("mesh"), |ui| {
                ui.label(egui::RichText::new("Position").size(10.5).color(t::TEXT_DIM));
                for (lab, v) in [("X", &mut pos.x), ("Y", &mut pos.y), ("Z", &mut pos.z)] {
                    let r = ui.add(egui::Slider::new(v, -50.0..=50.0).text(lab));
                    xform_changed |= r.changed();
                    released |= r.drag_stopped() || r.lost_focus();
                }
                ui.label(egui::RichText::new("Scale").size(10.5).color(t::TEXT_DIM));
                for (lab, v) in [("X", &mut scale.x), ("Y", &mut scale.y), ("Z", &mut scale.z)] {
                    let r = ui.add(egui::Slider::new(v, 0.01..=20.0).text(lab));
                    xform_changed |= r.changed();
                    released |= r.drag_stopped() || r.lost_focus();
                }
            });
            if xform_changed {
                if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                    node.local_position = pos;
                    node.local_scale = scale;
                }
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if released {
                self.transaction_snapshot_only(&format!("Move {kind}"));
            }
        }

        ui.add_space(4.0);
        if ui
            .button("Make Editable")
            .on_hover_text(
                "Bake parameters into the mesh. Topology becomes freely editable; \
                 Sides / Radius controls go away.",
            )
            .clicked()
        {
            self.poly_make_editable(nid);
            self.transaction_snapshot_only("Make Editable");
            self.toast("Primitive applied — mesh is now editable");
        }
        true
    }

    /// Live bevel params (Ctrl+B) — sole editable home for the modal (no floating Bevel window).
    /// Writes through `poly_bevel_set_*` so viewport cursor HUD + mesh stay in sync.
    fn draw_tool_params_poly_bevel(&mut self, ui: &mut egui::Ui) {
        let (mut width, mut segments, mut shape, mut clamp, mut consume, is_vert) =
            match self.tool.current.as_poly_bevel() {
                Some(d) => (
                    d.width,
                    d.segments as i32,
                    d.shape,
                    d.clamp,
                    d.consume,
                    !d.verts.is_empty(),
                ),
                None => return,
            };
        let title = if is_vert { "Bevel Vertices" } else { "Bevel" };
        ui.label(
            egui::RichText::new(format!("Live · {title}"))
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        ui.add_space(2.0);
        let mut w_ch = false;
        let mut s_ch = false;
        let mut sh_ch = false;
        let mut c_ch = false;
        let mut cons_ch = false;
        // Shape suffix matches former floating HUD (flat / round / convex).
        let shape_sfx = if shape < 0.2 {
            "flat"
        } else if shape < 0.8 {
            "round"
        } else {
            "convex"
        };
        f::section(ui, "live/Bevel/params", "Bevel", Some("edge"), |ui| {
            // Width has no hard upper bound (same as former viewport HUD) — clamp mesh-side.
            let r = f::num(ui, "Width", &mut width, 0.005, 0.0..=f32::INFINITY, None);
            w_ch |= r.changed();
            let r = f::int(ui, "Segments", &mut segments, 1..=32);
            s_ch |= r.changed();
            let r = f::num(ui, "Shape", &mut shape, 0.01, 0.0..=1.0, Some(shape_sfx));
            sh_ch |= r.changed();
            let r = f::bool_row(ui, "Clamp overlap", &mut clamp)
                .on_hover_text("Limit width so faces can't invert (Blender Clamp Overlap / C).");
            c_ch |= r.changed();
            // Re-homed from the deleted floating Bevel window (master added it there while
            // this branch was removing that panel) — the dock is now its only home.
            let r = f::bool_row(ui, "Consume geometry", &mut consume).on_hover_text(
                "SHIFT toggles this while the bevel is live. CAD-style: when the width reaches \
                 the next edge loop, absorb it and keep growing instead of sticking at the clamp. \
                 Honours Segments/Shape. Non-destructive until confirm (shrink the width to bring \
                 rings back).",
            );
            cons_ch |= r.changed();
        });
        ui.label(
            egui::RichText::new(
                "drag=width · ALT+drag=shape · scroll=segments (need >1 for round) · SHIFT=consume · C=clamp",
            )
            .size(10.0)
            .color(t::TEXT_FAINT),
        );
        if w_ch {
            self.poly_bevel_set_width(width);
        }
        if s_ch {
            self.poly_bevel_set_segments(segments.max(1) as u32);
        }
        if sh_ch {
            self.poly_bevel_set_shape(shape);
        }
        if c_ch {
            // Toggle only if different — poly_bevel_toggle_clamp flips.
            if self
                .tool
                .current
                .as_poly_bevel()
                .map(|d| d.clamp != clamp)
                .unwrap_or(false)
            {
                self.poly_bevel_toggle_clamp();
            }
        }
        if cons_ch {
            // Same flip-guard as clamp — poly_bevel_toggle_consume flips, it does not set.
            if self
                .tool
                .current
                .as_poly_bevel()
                .map(|d| d.consume != consume)
                .unwrap_or(false)
            {
                self.poly_bevel_toggle_consume();
            }
        }
    }

    fn draw_tool_params_poly_extrude(&mut self, ui: &mut egui::Ui) {
        let (mut offset, title, is_edge, along_normals, grouped, force_planar) =
            if let Some(d) = self.tool.current.as_poly_extrude() {
                (
                    d.accumulated_offset,
                    if d.along_normals {
                        "Extrude Along Normals"
                    } else {
                        "Extrude"
                    },
                    false,
                    d.along_normals,
                    d.along_normals_grouped,
                    d.force_planar,
                )
            } else if let Some(d) = self.tool.current.as_poly_edge_extrude() {
                (d.accumulated_offset, "Edge Extrude", true, false, false, false)
            } else {
                return;
            };
        ui.label(
            egui::RichText::new(format!("Live · {title}"))
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        // Keep the palette / active-tool bus on the real tool name while live.
        if !is_edge {
            self.ui2.active_tool = title.into();
        }

        if along_normals {
            // Dedicated Along Normals panel (owner: was showing plain Extrude only).
            f::section(ui, "live/ExtrudeAlongN/mode", "Mode", None, |ui| {
                ui.horizontal(|ui| {
                    let mut want_grouped = grouped;
                    if ui
                        .selectable_label(!want_grouped, "Individual")
                        .on_hover_text("Each face along its own normal (I). Clean walls.")
                        .default_menu("poly.extrude_along_normals_grouped", serde_json::json!(false))
                        .clicked()
                    {
                        want_grouped = false;
                    }
                    if ui
                        .selectable_label(want_grouped, "Grouped")
                        .on_hover_text("One connected shell; verts blend normals (G).")
                        .default_menu("poly.extrude_along_normals_grouped", serde_json::json!(true))
                        .clicked()
                    {
                        want_grouped = true;
                    }
                    if want_grouped != grouped {
                        self.poly_extrude_set_along_group(want_grouped);
                    }
                });
                // Owner FORCE STRAITH FACE (2026-08-05): keep caps planar — no bent n-gons.
                let mut want_planar = force_planar;
                if ui
                    .checkbox(&mut want_planar, "Force planar faces")
                    .on_hover_text(
                        "Keep each extruded cap straight (face-constant normals + flatten).\n\
                         Off = free along blended normals (can bend / twist faces).",
                    )
                    .default_menu("poly.extrude_force_planar", serde_json::json!(want_planar))
                    .changed()
                {
                    self.poly_extrude_set_force_planar(want_planar);
                }
            });
            f::section(ui, "live/ExtrudeAlongN/params", "Depth", None, |ui| {
                let prev = self
                    .tool
                    .current
                    .as_poly_extrude()
                    .map(|d| d.accumulated_offset)
                    .unwrap_or(offset);
                let r = f::num(ui, "Depth", &mut offset, 0.01, -100.0..=100.0, None);
                if r.changed() {
                    // Apply the delta so the mesh tracks the field (not only the stored number).
                    if let Some(d) = self.tool.current.as_poly_extrude() {
                        let nid = d.node_id;
                        let faces = d.new_face_ids.clone();
                        let verts = d.vert_normals.clone();
                        let avg = {
                            let sum: glam::Vec3 =
                                d.face_normals.iter().map(|(_, n)| *n).sum();
                            sum.normalize_or_zero()
                        };
                        let delta = offset - prev;
                        self.poly_extrude_apply_delta(nid, &faces, avg, &verts, true, delta);
                        if let Some(d) = self.tool.current.as_poly_extrude_mut() {
                            d.accumulated_offset = offset;
                        }
                        self.poly_sync_after_position_edit(nid);
                        self.mark_dirty(crate::DirtyFlags::RENDER);
                    }
                }
            });
            self.draw_extrude_material_controls(ui);
            ui.label(
                egui::RichText::new(
                    "G=Grouped · I=Individual · Force planar · N=Region Normal · drag · Enter/LMB confirm · Esc cancel",
                )
                .size(10.0)
                .color(t::TEXT_FAINT),
            );
            return;
        }

        f::section(ui, "live/Extrude/params", title, None, |ui| {
            let prev = if is_edge {
                self.tool
                    .current
                    .as_poly_edge_extrude()
                    .map(|d| d.accumulated_offset)
                    .unwrap_or(offset)
            } else {
                self.tool
                    .current
                    .as_poly_extrude()
                    .map(|d| d.accumulated_offset)
                    .unwrap_or(offset)
            };
            let r = f::num(ui, "Depth", &mut offset, 0.01, -100.0..=100.0, None);
            if r.changed() {
                if is_edge {
                    if let Some(d) = self.tool.current.as_poly_edge_extrude_mut() {
                        d.accumulated_offset = offset;
                    }
                } else if let Some(d) = self.tool.current.as_poly_extrude() {
                    let nid = d.node_id;
                    let faces = d.new_face_ids.clone();
                    let verts = d.vert_normals.clone();
                    let along = d.along_normals;
                    let avg = {
                        let sum: glam::Vec3 = d.face_normals.iter().map(|(_, n)| *n).sum();
                        sum.normalize_or_zero()
                    };
                    let delta = offset - prev;
                    self.poly_extrude_apply_delta(nid, &faces, avg, &verts, along, delta);
                    if let Some(d) = self.tool.current.as_poly_extrude_mut() {
                        d.accumulated_offset = offset;
                    }
                    self.poly_sync_after_position_edit(nid);
                    self.mark_dirty(crate::DirtyFlags::RENDER);
                }
            }
            if !is_edge {
                // Stacked-wall cleanup (persisted). On = merge new walls into neighbouring
                // coplanar planes on confirm (SketchUp push/pull). Off keeps the raw extrude.
                let mut mode = if self.tweak.extrude_wall_cleanup == 0 { 0u8 } else { 2 };
                egui::ComboBox::from_label("Wall cleanup")
                    .selected_text(if mode == 0 { "Off" } else { "On" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut mode, 2, "On");
                        ui.selectable_value(&mut mode, 0, "Off");
                    });
                if mode != self.tweak.extrude_wall_cleanup {
                    self.tweak.extrude_wall_cleanup = mode;
                }

                // Push-through DEBRIS merge — this tool's OWN cleanup knob, deliberately separate
                // from "Wall cleanup" above (that one only collapses stacked coplanar walls) and
                // from the boolean kernel's internal epsilon pass, which is capped far below the
                // scale a real intrude leaves behind. See `poly_extrude_merge_debris`.
                let mut debris = self.tweak.extrude_debris_merge;
                let resp = ui
                    .add(
                        egui::Slider::new(&mut debris, 0.0..=1e-3)
                            .text("Debris merge")
                            .custom_formatter(|v, _| {
                                if v <= 0.0 { "Off".into() } else { format!("{:.2}e-4", v * 1e4) }
                            }),
                    )
                    .on_hover_text(
                        "Weld the near-duplicate vertices a through-cut leaves where it grazes an \
                         existing corner \u{2014} they shatter the flat side planes into triangle \
                         fans and compound with every cut.\n\
                         Radius as a fraction of the mesh size. 0 = Off. \
                         Authored edges (bevel tags, sharp, seam, crease) are never welded.",
                    );
                if resp.changed() {
                    self.tweak.extrude_debris_merge = debris.clamp(0.0, 1e-3);
                }

                // Material: default inherit source face; optional chosen material for new walls.
                self.draw_extrude_material_controls(ui);

                // Quick path into Along Normals from the region Extrude panel.
                if ui
                    .button("Along Normals…")
                    .on_hover_text("N — each face along its normal (or start with Alt+E)")
                    .clicked()
                {
                    self.poly_extrude_toggle_along_normals();
                }
            }
        });
        ui.label(
            egui::RichText::new(
                if is_edge {
                    "Move mouse in viewport · Enter/LMB confirm · Esc cancel"
                } else {
                    "N=Along Normals · Shift=world axis · D=edge dir · Enter/LMB confirm · Esc cancel"
                },
            )
            .size(10.0)
            .color(t::TEXT_FAINT),
        );
    }

    /// Extrude material controls: default inherit source-face assignment; optional chosen mat.
    fn draw_extrude_material_controls(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.label(
            egui::RichText::new("Material")
                .size(10.0)
                .color(t::TEXT_SECONDARY),
        );
        let mut use_chosen = self.tweak.extrude_use_chosen_material;
        if ui
            .checkbox(&mut use_chosen, "Use chosen material")
            .on_hover_text(
                "Off (default): new walls inherit each source face's assigned material.\n\
                 On: paint the material below onto new walls / extruded faces.",
            )
            .changed()
        {
            self.tweak.extrude_use_chosen_material = use_chosen;
            self.tweak.save();
        }
        if !use_chosen {
            ui.label(
                egui::RichText::new("Walls use the source face material")
                    .size(10.0)
                    .color(t::TEXT_FAINT),
            );
            return;
        }
        // Material picker from scene materials + body default (0).
        let mats: Vec<(u32, String)> = {
            let mut out = vec![(0u32, "Body default (0)".into())];
            for (_nid, mid, name, _data) in self.scene.scene_graph.materials() {
                let label = if name.is_empty() {
                    format!("Material {}", mid.0)
                } else {
                    name.to_string()
                };
                out.push((mid.0, label));
            }
            out.sort_by(|a, b| a.0.cmp(&b.0));
            out
        };
        let mut mid = self.tweak.extrude_chosen_material_id;
        let current_label = mats
            .iter()
            .find(|(id, _)| *id == mid)
            .map(|(_, n)| n.clone())
            .unwrap_or_else(|| format!("Material {mid}"));
        egui::ComboBox::from_label("Extrude material")
            .selected_text(current_label)
            .show_ui(ui, |ui| {
                for (id, name) in &mats {
                    if ui.selectable_label(mid == *id, name).clicked() {
                        mid = *id;
                    }
                }
            });
        if mid != self.tweak.extrude_chosen_material_id {
            self.tweak.extrude_chosen_material_id = mid;
            self.tweak.save();
        }
    }

    fn draw_tool_params_poly_shrink_fatten(&mut self, ui: &mut egui::Ui) {
        let mut offset = match self.tool.current.as_poly_shrink_fatten() {
            Some(d) => d.accumulated_offset,
            None => return,
        };
        ui.label(
            egui::RichText::new("Live · Shrink/Fatten")
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        f::section(ui, "live/ShrinkFatten/params", "Offset", None, |ui| {
            let r = f::num(ui, "Offset", &mut offset, 0.01, -100.0..=100.0, None);
            if r.changed() {
                if let Some(d) = self.tool.current.as_poly_shrink_fatten_mut() {
                    d.accumulated_offset = offset;
                }
            }
        });
        ui.label(
            egui::RichText::new("drag = offset · Ctrl = snap · Enter confirm")
                .size(10.0)
                .color(t::TEXT_FAINT),
        );
    }

    /// Live readout for the `W` edge-weight drag. Percent, because that is the unit the Shift stops
    /// are defined in (20/40/60/80/100) — the stored column is 0..1.
    fn draw_tool_params_poly_edge_weight(&mut self, ui: &mut egui::Ui) {
        let (mut pct, n) = match self.tool.current.as_poly_edge_weight() {
            Some(d) => (d.weight * 100.0, d.keys.len()),
            None => return,
        };
        ui.label(
            egui::RichText::new("Live · Edge Weight")
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        f::section(ui, "live/EdgeWeight/params", "Weight", None, |ui| {
            let r = f::num(ui, "Weight %", &mut pct, 1.0, 0.0..=100.0, None);
            if r.changed() {
                let w = (pct / 100.0).clamp(0.0, 1.0);
                // A value set here also becomes the new drag base AND re-anchors the drag, so moving
                // the cursor afterwards climbs from this value instead of snapping to wherever the
                // accumulated travel from the arm point had reached. It must also clear `num_buf`:
                // a stale typed value OVERRIDES the mouse every frame, so leaving it set would let the
                // next update stomp what was just entered here.
                let anchor = self.viewport.viewport_mouse;
                let target = {
                    let mut out = None;
                    if let Some(d) = self.tool.current.as_poly_edge_weight_mut() {
                        d.weight = w;
                        d.base = w;
                        d.start_mouse = anchor;
                        d.num_buf.clear();
                        let value = if w <= 0.0 { None } else { Some(w) };
                        let entries: Vec<_> =
                            d.keys.iter().map(|&(k, _)| (k, value)).collect();
                        out = Some((d.node_id, entries));
                    }
                    out
                };
                if let Some((nid, entries)) = target {
                    self.poly_write_edge_weights(nid, &entries);
                }
            }
        });
        ui.label(
            egui::RichText::new(format!(
                "{n} edge{} · drag = strength · type a % · Shift = 20% steps · Ctrl = reset 0 · click/Enter accept · RMB/Esc cancel",
                if n == 1 { "" } else { "s" }
            ))
            .size(10.0)
            .color(t::TEXT_FAINT),
        );
    }

    fn draw_tool_params_poly_slide(&mut self, ui: &mut egui::Ui) {
        let (mut factor, mut clamp, title) = if let Some(d) = self.tool.current.as_poly_edge_slide()
        {
            (d.factor, d.clamp, "Edge Slide")
        } else if let Some(d) = self.tool.current.as_poly_vert_slide() {
            (d.factor, d.clamp, "Vertex Slide")
        } else {
            return;
        };
        ui.label(
            egui::RichText::new(format!("Live · {title}"))
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        f::section(ui, "live/Slide/params", "Slide", None, |ui| {
            let _ = f::num(ui, "Factor", &mut factor, 0.01, -1.0..=1.0, None);
            let _ = f::bool_row(ui, "Clamp", &mut clamp);
            if let Some(d) = self.tool.current.as_poly_edge_slide_mut() {
                d.factor = factor;
                d.clamp = clamp;
            } else if let Some(d) = self.tool.current.as_poly_vert_slide_mut() {
                d.factor = factor;
                d.clamp = clamp;
            }
        });
    }

    fn draw_tool_params_poly_transform(&mut self, ui: &mut egui::Ui) {
        use crate::{AxisConstraint, TransformKind};
        let Some(ts) = self.tool.current.as_poly_transform() else {
            return;
        };
        let kind_str = match ts.kind {
            TransformKind::Grab => "Grab",
            TransformKind::Rotate => "Rotate",
            TransformKind::Scale => "Scale",
            TransformKind::Elongate => "Elongate",
        };
        ui.label(
            egui::RichText::new(format!("Live · {kind_str}"))
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        f::section(ui, "live/PolyXform/params", "Transform", None, |ui| {
            ui.label(format!("Mode: {kind_str}"));
            if ts.axis != AxisConstraint::None {
                let axis_str = match ts.axis {
                    AxisConstraint::X => "X",
                    AxisConstraint::Y => "Y",
                    AxisConstraint::Z => "Z",
                    AxisConstraint::PlaneX => "YZ plane",
                    AxisConstraint::PlaneY => "XZ plane",
                    AxisConstraint::PlaneZ => "XY plane",
                    AxisConstraint::None => "",
                };
                ui.label(format!("Axis: {axis_str}"));
            }
            if !ts.num_buf.is_empty() {
                ui.label(format!("Value: {}", ts.num_buf));
            }
            if let crate::FalloffKind::Proportional { radius, .. } = ts.falloff {
                ui.label(format!("Proportional radius: {radius:.3}"));
            }
        });
        ui.label(
            egui::RichText::new("X/Y/Z = axis · type number · Enter/LMB confirm · Esc cancel")
                .size(10.0)
                .color(t::TEXT_FAINT),
        );
    }

    fn draw_tool_params_object_transform(&mut self, ui: &mut egui::Ui) {
        use crate::TransformKind;
        let Some(ts) = self.tool.current.as_transform() else {
            return;
        };
        let kind_str = match ts.kind {
            TransformKind::Grab => "Move",
            TransformKind::Rotate => "Rotate",
            TransformKind::Scale => "Scale",
            TransformKind::Elongate => "Elongate",
        };
        ui.label(
            egui::RichText::new(format!("Live · {kind_str}"))
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        f::section(ui, "live/ObjXform/params", "Transform", None, |ui| {
            ui.label(format!("Mode: {kind_str}"));
            if ts.axis != crate::AxisConstraint::None {
                let axis_str = match ts.axis {
                    crate::AxisConstraint::X => "X",
                    crate::AxisConstraint::Y => "Y",
                    crate::AxisConstraint::Z => "Z",
                    crate::AxisConstraint::PlaneX => "YZ plane",
                    crate::AxisConstraint::PlaneY => "XZ plane",
                    crate::AxisConstraint::PlaneZ => "XY plane",
                    crate::AxisConstraint::None => "",
                };
                ui.label(format!("Axis: {axis_str}"));
            }
            if !ts.num_buf.is_empty() {
                ui.label(format!("Value: {}", ts.num_buf));
            }
        });
        ui.label(
            egui::RichText::new("X/Y/Z = axis · type number · Enter confirm · Esc cancel")
                .size(10.0)
                .color(t::TEXT_FAINT),
        );
    }

    fn draw_tool_params_create_live(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new("Live · Create")
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        let (live_w, live_d, live_h) = if let Some(cs) = self.tool.current.as_create() {
            (cs.local_u.abs(), cs.local_v.abs(), cs.depth.abs())
        } else {
            (
                self.tool.create_tool_settings.width,
                self.tool.create_tool_settings.depth,
                self.tool.create_tool_settings.height,
            )
        };
        f::section(ui, "live/Create/dims", "Dimensions", Some("local"), |ui| {
            ui.label(format!("W {:.3}  D {:.3}  H {:.3}", live_w, live_d, live_h));
            let mut w = self.tool.create_tool_settings.width;
            let mut d = self.tool.create_tool_settings.depth;
            let mut h = self.tool.create_tool_settings.height;
            let r1 = f::num(ui, "Lock width", &mut w, 0.01, 0.001..=100.0, Some("u"));
            let r2 = f::num(ui, "Lock depth", &mut d, 0.01, 0.001..=100.0, Some("u"));
            let r3 = f::num(ui, "Lock height", &mut h, 0.01, 0.001..=100.0, Some("u"));
            if r1.changed() {
                self.tool.create_tool_settings.width = w;
                self.tool.create_tool_settings.lock_width = true;
            }
            if r2.changed() {
                self.tool.create_tool_settings.depth = d;
                self.tool.create_tool_settings.lock_depth = true;
            }
            if r3.changed() {
                self.tool.create_tool_settings.height = h;
                self.tool.create_tool_settings.lock_height = true;
            }
        });
        ui.label(
            egui::RichText::new("Click-drag in viewport to place · locks pin W/D/H")
                .size(10.0)
                .color(t::TEXT_FAINT),
        );
    }

    fn draw_tool_params_nurbs_fillet_live(&mut self, ui: &mut egui::Ui) {
        let Some(d) = self.tool.current.as_nurbs_fillet() else {
            return;
        };
        let mut peak = d.radius.peak();
        let max_r = d.max_radius.max(0.02);
        let n_edges = d.edges.len();
        ui.label(
            egui::RichText::new("Live · Fillet")
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        f::section(ui, "live/NurbsFillet/params", "Fillet", None, |ui| {
            let resp = f::num(ui, "Peak radius", &mut peak, 0.001, 0.0..=max_r, None);
            if resp.changed() {
                if let Some(d) = self.tool.current.as_nurbs_fillet_mut() {
                    let p = peak.clamp(0.0, d.max_radius.max(0.02));
                    d.radius = d.radius.with_peak(p);
                }
            }
            ui.label(format!("{n_edges} edge(s) · max {max_r:.3}"));
        });
        ui.label(
            egui::RichText::new("drag = radius · V = law · Enter confirm · Esc cancel")
                .size(10.0)
                .color(t::TEXT_FAINT),
        );
    }

    /// True when Bake Mesh Maps is the armed tool. Selecting a poly is not enough.
    pub(crate) fn tool_params_mesh_maps_active(&self) -> bool {
        self.ui2.active_tool == "Mesh Maps" && self.poly_curv_body().is_some()
    }

    // draw_tool_params_sculpt lives in tool_params_sculpt.rs (full Blender header set).

    fn draw_tool_params_poly_inset(&mut self, ui: &mut egui::Ui) {
        let (mut thick, mut depth) = match self.tool.current.as_poly_inset() {
            Some(d) => (d.accumulated_amount, d.accumulated_depth),
            None => return,
        };
        ui.label(
            egui::RichText::new("Live · Inset")
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        f::section(ui, "live/Inset/params", "Inset", None, |ui| {
            // Negative = outer rim segment (body fixed); positive = classic inward inset.
            let _ = f::num(ui, "Thickness", &mut thick, 0.01, -100.0..=100.0, None);
            let _ = f::num(ui, "Depth", &mut depth, 0.01, -100.0..=100.0, None);
            if let Some(d) = self.tool.current.as_poly_inset_mut() {
                let changed = (d.accumulated_amount - thick).abs() > 1e-9
                    || (d.accumulated_depth - depth).abs() > 1e-9;
                d.accumulated_amount = thick;
                d.accumulated_depth = depth;
                if changed {
                    let nid = d.node_id;
                    let ring = d.original_positions.clone();
                    let dirs = d.offset_dirs.clone();
                    let amount = thick;
                    let depth_n = d.depth_normal;
                    let dep = depth;
                    self.poly_with_base_mesh_mut(nid, |mesh| {
                        for (i, &(vid, orig)) in ring.iter().enumerate() {
                            if i < dirs.len() && (vid as usize) < mesh.vertices.len() {
                                mesh.vert_mut(core_math::half_edge::VertexId(vid)).position =
                                    orig + dirs[i] * amount + depth_n * dep;
                            }
                        }
                    });
                    self.poly_inset_ensure_topology(amount);
                    self.poly_sync_after_position_edit(nid);
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            }
        });
    }

    /// MESHmachine-style Loop Select parameters (Edge mode, idle).
    ///
    /// Matches the MACHIN3: Select panel: Always Loop Select, Loop Select mode, Min Angle,
    /// Use Edge Limit, Steps. Drives Alt+click edge loop and Alt+Ctrl edge ring.
    fn draw_tool_params_loop_select(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new("Loop Select (Alt+click)")
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        f::section(ui, "live/LoopSelect/params", "Loop Select", None, |ui| {
            f::bool_row(ui, "Always Loop Select", &mut self.loop_sel_always_loop)
                .on_hover_text(
                    "On: always use angle-based loop select (LSelect).\n\
                     Off: if the whole selection is Mark Sharp edges, grow along sharps \
                     instead (MeshMachine SSelect branch).",
                );
            // Mode row (MeshMachine shows Loop Select as the active mode button).
            ui.horizontal(|ui| {
                let active = egui::Color32::from_rgb(50, 100, 180);
                let btn = egui::Button::new(
                    egui::RichText::new("Loop Select").color(egui::Color32::WHITE),
                )
                .fill(active)
                .min_size(egui::vec2(100.0, 22.0));
                let _ = ui.add(btn).on_hover_text(
                    "Alt+LMB = edge loop · Alt+Ctrl+LMB = edge ring · Shift adds",
                );
            });
            f::num(
                ui,
                "Min Angle",
                &mut self.loop_sel_min_angle,
                1.0,
                1.0..=179.0,
                Some("°"),
            )
            .on_hover_text(
                "Maximum turn from straight when growing the loop (degrees).\n\
                 Higher values grow the selection further (MeshMachine 0.8+).",
            );
            f::bool_row(ui, "Use Edge Limit", &mut self.loop_sel_use_edge_limit)
                .on_hover_text(
                    "Cap how far the loop/ring walks from the seed edge (per direction).",
                );
            if self.loop_sel_use_edge_limit {
                let mut steps = self.loop_sel_steps as i32;
                if f::int(ui, "Steps", &mut steps, 1..=256).changed() {
                    self.loop_sel_steps = steps.max(1) as u32;
                }
            }
        });
        ui.label(
            egui::RichText::new(
                "Alt+click loop · Alt+Shift directed (2 edges) · Alt+Ctrl ring · Shift adds",
            )
            .size(10.0)
            .color(t::TEXT_DIM),
        );
    }

    /// Loop Cut parameters.
    ///
    /// Shown from the moment Ctrl+R is ARMED, not only once a slide session exists. The session is
    /// created by `poly_loop_cut_session_tick` on the first hovered edge, so gating the panel on it
    /// meant that pressing Ctrl+R with the cursor off the geometry showed no parameters at all —
    /// which reads as "this tool has none" (owner 2026-08-10). With no session the panel edits the
    /// tool-settings defaults, which the session then latches.
    fn draw_tool_params_poly_loop_cut(&mut self, ui: &mut egui::Ui) {
        let live_session = self.tool.current.is_poly_loop_cut_slide();
        let (mut cuts, mut factor, mut clamp, mut paired, mut live) =
            match self.tool.current.as_poly_loop_cut_slide() {
                Some(d) => (d.count as i32, d.factor, d.clamp, d.paired, d.live),
                // Armed but not seeded: the defaults a session would start from.
                None => (self.loop_cut_count.max(1) as i32, 0.0, true, false, self.loop_cut_live),
            };
        let mut selected = self.loop_cut_selected;
        let mut follow = self.loop_cut_follow_curvature;
        let mut curve_amt = self.loop_cut_curvature;
        ui.label(
            egui::RichText::new(if live_session { "Live · Loop Cut" } else { "Loop Cut (Ctrl+R)" })
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        let mut changed = false;
        let mut selected_changed = false;
        f::section(ui, "live/LoopCut/params", "Loop Cut", None, |ui| {
            changed |= f::int(ui, "Cuts", &mut cuts, 1..=20).changed();
            if live_session {
                changed |= f::num(ui, "Slide", &mut factor, 0.01, -1.0..=1.0, None).changed();
                changed |= f::bool_row(ui, "Clamp", &mut clamp).changed();
                changed |= f::bool_row(ui, "Paired", &mut paired).changed();
            }
            changed |= f::bool_row(ui, "Live", &mut live).changed();
            let sel_resp = f::bool_row(ui, "Selected", &mut selected);
            sel_resp.clone().on_hover_text(
                "On: selected edges drive the cut — multi-ring (one loop per independent ring) \
                 and partial span when several edges of the same ring are selected.\n\
                 Off: free full loop from the hovered/seed edge only. Selection is ignored, so \
                 the strip can walk through n-gons and you keep control of a single ring even \
                 with edges selected.",
            );
            selected_changed = sel_resp.changed();
            changed |= selected_changed;
            let follow_resp = f::bool_row(ui, "Follow Curvature", &mut follow);
            follow_resp.clone().on_hover_text(
                "Off: the new loop sits on the straight line between the edges it crosses, which \
                 flattens a rounded form.\nOn: each new vertex is placed on the arc through those \
                 edges' endpoint normals, so the loop follows the curvature.\nFlat faces are \
                 unaffected either way.",
            );
            changed |= follow_resp.changed();
            if follow {
                changed |= f::num(ui, "Curvature", &mut curve_amt, 0.01, 0.0..=2.0, None).changed();
            }
        });
        if !changed {
            return;
        }
        // Tool settings persist past this session (next Ctrl+R starts from them).
        self.loop_cut_count = cuts.max(1) as u32;
        self.loop_cut_live = live;
        self.loop_cut_selected = selected;
        self.loop_cut_follow_curvature = follow;
        self.loop_cut_curvature = curve_amt;
        // Selected changes which rings exist — re-seed the session so the preview matches.
        if selected_changed && live_session {
            self.poly_loop_cut_reseed_session();
            self.mark_dirty(DirtyFlags::RENDER);
            return;
        }
        let curvature = self.loop_cut_effective_curvature();
        if let Some(d) = self.tool.current.as_poly_loop_cut_slide_mut() {
            d.count = cuts.max(1) as u32;
            d.factor = factor;
            d.clamp = clamp;
            d.paired = paired;
            d.live = live;
            d.curvature = curvature;
        }
        // A parameter edit must show immediately — in live mode that means re-cutting now, not on
        // the next drag. (Preview mode redraws from the same values, so RENDER is enough.)
        if live_session && live {
            self.poly_loop_cut_regenerate();
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Auto-accept (Done) last-create when the user does anything else afterward.
    ///
    /// Sticky "Adjust box" + Done after a later edit was wrong: once work continues
    /// (other selection, modal tool, component edit, another last-op), create adjust
    /// is committed and cleared — same lifecycle as Blender's F9 redo panel.
    pub(crate) fn poly_create_adjust_auto_accept_if_stale(&mut self) {
        let Some(a) = self.ui.poly_create_adjust.as_ref() else {
            return;
        };
        let nid = a.nid;
        let kind = a.spec.label().to_string();
        let needs_settle = a.needs_settle;

        let node_gone = self.scene.scene_graph.get(nid).is_none();
        let selection_moved = self.sel.usel.active_node() != Some(nid);
        let modal_busy = self.tool.current.is_busy();
        let component_edit = self.poly_work_mode != crate::PolyWorkMode::Object;
        let other_last_op = self.ui.poly_bevel_adjust.is_some()
            || self.ui.poly_inset_adjust.is_some()
            || self.ui.poly_circle_adjust.is_some()
            || self.ui.poly_tosphere_adjust.is_some()
            || self.ui.poly_boolean_adjust.is_some()
            || self.ui.nurbs_fillet_adjust.is_some();

        if !(node_gone || selection_moved || modal_busy || component_edit || other_last_op) {
            return;
        }

        if !node_gone {
            self.poly_request_display_sync(nid);
            if needs_settle {
                self.transaction_snapshot_only(&format!("Adjust {kind}"));
            }
            if let Some(a) = self.ui.poly_create_adjust.as_ref() {
                if let Some(sides) = crate::creation::spec_sides_to_remember(&a.spec) {
                    self.ui.poly_create_last_sides = sides;
                }
            }
        }
        self.ui.poly_create_adjust = None;
    }

    /// Last-create adjust (box/sphere/…) — sole UI; floating duplicate was removed.
    fn draw_tool_params_poly_create_adjust(&mut self, ui: &mut egui::Ui) {
        use core_math::scene_graph::NodePayload;
        // Re-check dismiss (selection / mode may have changed this frame).
        self.poly_create_adjust_auto_accept_if_stale();
        let mut a = match self.ui.poly_create_adjust.clone() {
            Some(a) => a,
            None => return,
        };
        if self.scene.scene_graph.get(a.nid).is_none() {
            self.ui.poly_create_adjust = None;
            return;
        }

        ui.label(
            egui::RichText::new(format!("Last · Create · {}", a.spec.label()))
                .size(11.0)
                .color(t::TEXT_SECONDARY),
        );
        ui.add_space(2.0);

        let mut geom_changed = false;
        let mut xform_changed = false;
        let mut released = false;

        f::section(ui, "live/PolyCreate/geom", "Geometry", Some("mesh"), |ui| {
            // ONE parameter widget, shared with the properties panel — see
            // `poly::primitive_params_ui`. The per-kind slider block that used to live here was
            // the third hand-maintained copy of the same parameter list.
            let e = crate::poly::primitive_params_ui::draw_primitive_spec_params(ui, &mut a.spec);
            geom_changed |= e.changed;
            released |= e.released;
        });

        f::section(ui, "live/PolyCreate/place", "Placement", None, |ui| {
            ui.label(egui::RichText::new("Position").size(10.5).color(t::TEXT_DIM));
            for (lab, v) in [("X", &mut a.pos.x), ("Y", &mut a.pos.y), ("Z", &mut a.pos.z)] {
                let r = ui.add(egui::Slider::new(v, -10.0..=10.0).text(lab));
                xform_changed |= r.changed();
                released |= r.drag_stopped() || r.lost_focus();
            }
            ui.label(egui::RichText::new("Scale").size(10.5).color(t::TEXT_DIM));
            for (lab, v) in [("X", &mut a.scale.x), ("Y", &mut a.scale.y), ("Z", &mut a.scale.z)] {
                let r = ui.add(egui::Slider::new(v, 0.01..=10.0).text(lab));
                xform_changed |= r.changed();
                released |= r.drag_stopped() || r.lost_focus();
            }
        });

        if (geom_changed || xform_changed) && a.place_on.is_some() {
            let place = a.place_on.unwrap();
            a.pos = crate::creation::poly_create_pos_on_surface(&a, &place);
            xform_changed = true;
        }

        if geom_changed {
            a.needs_settle = true;
            if let Some(sides) = crate::creation::spec_sides_to_remember(&a.spec) {
                self.ui.poly_create_last_sides = sides;
            }
            // THE spec mutator: writes the spec, rebuilds the mesh, and keeps the content
            // signature in step so the body stays recognizably parametric.
            self.set_primitive_spec(a.nid, a.spec);
        }
        if xform_changed {
            a.needs_settle = true;
            if let Some(node) = self.scene.scene_graph.get_mut(a.nid) {
                node.local_position = a.pos;
                node.local_rotation = a.rot;
                node.local_scale = a.scale;
            }
            self.mark_dirty(crate::DirtyFlags::RENDER);
        }

        self.ui.poly_create_adjust = Some(a.clone());
        if released && a.needs_settle {
            self.poly_request_display_sync(a.nid);
            self.transaction_snapshot_only(&format!("Adjust {}", a.spec.label()));
            if let Some(adj) = self.ui.poly_create_adjust.as_mut() {
                adj.needs_settle = false;
            }
        }

        ui.add_space(4.0);
        if ui.button("Done").clicked() {
            self.poly_request_display_sync(a.nid);
            if let Some(sides) = crate::creation::spec_sides_to_remember(&a.spec) {
                self.ui.poly_create_last_sides = sides;
            }
            self.ui.poly_create_adjust = None;
        }
    }

    /// Reset shape / bevel / boolean / transform params to create defaults.
    /// Keeps name, uid, position, visibility, material.
    fn reset_prim_to_defaults(&mut self, idx: usize) {
        use core_math::primitives::{BooleanOp, BoxBevel, CylinderBevel, SdfShape};
        use glam::Vec3;
        if idx >= self.scene.primitives.len() {
            return;
        }
        let shape_idx = match &self.scene.primitives[idx].shape {
            SdfShape::Sphere { .. } => 0,
            SdfShape::Box { .. } | SdfShape::Stair { .. } => 1,
            SdfShape::Cylinder { .. } => 2,
            SdfShape::Torus { .. } => 3,
            SdfShape::Cone { .. } => 4,
            SdfShape::Capsule { .. } => 5,
            SdfShape::SpiralStair { .. } => 7,
            _ => 1,
        };
        // Undoable one-shot: transaction() snapshots the graph BEFORE the closure
        // mutates the flat cache + mirrors, so Ctrl+Z restores the pre-reset prim.
        self.transaction("Reset parameters", |s| {
            let new_shape = Self::shape_preset(shape_idx);
            let p = &mut s.scene.primitives[idx];
            p.shape = new_shape;
            p.rotation_deg = Vec3::ZERO;
            p.scale = Vec3::ONE;
            p.operation = BooleanOp::Union { k: 0.15 };
            p.elongation = Vec3::ZERO;
            p.cyl_bevel = CylinderBevel::default();
            p.bevel_profile_top = 0.0;
            p.bevel_profile_bot = 0.0;
            p.bevel_profile_vert = 0.0;
            p.chamfer_angle_top = 45.0;
            p.chamfer_angle_bot = 45.0;
            p.chamfer_angle_vert = 45.0;
            // Ensure box bevel is default (shape_preset already does for Box).
            if let SdfShape::Box { bevel, .. } = &mut p.shape {
                *bevel = BoxBevel::default();
            }
            s.mirror_prim_to_graph(idx);
            if s.is_master(idx) {
                let uid = s.scene.primitives[idx].prim_uid;
                s.sync_instances_of(uid);
            }
            s.rebake_no_undo();
        });
        self.toast("Parameters reset to defaults");
    }

    fn draw_tool_params_bound(&mut self, ui: &mut egui::Ui, idx: usize) {
        use core_math::primitives::SdfShape;

        let tool = tool_name_from_shape(&self.scene.primitives[idx].shape);
        let pillar = "sdf";
        let name = self.scene.primitives[idx].name.clone();
        let mut do_reset = false;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{name}  ·  {tool}"))
                    .size(11.0)
                    .color(t::TEXT_SECONDARY),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Reset")
                                .size(10.5)
                                .color(t::TEXT_DIM),
                        )
                        .fill(t::SURFACE_FIELD)
                        .stroke(egui::Stroke::new(1.0, t::SEAM))
                        .rounding(4.0)
                        .min_size(egui::vec2(52.0, 20.0)),
                    )
                    .on_hover_text(
                        "Reset shape, bevel, boolean & transform to defaults (keeps position)",
                    )
                    .clicked()
                {
                    do_reset = true;
                }
            });
        });
        if do_reset {
            self.reset_prim_to_defaults(idx);
            return;
        }
        ui.add_space(2.0);

        let tp = match tool_params(tool, pillar) {
            Some(tp) => tp,
            None => {
                empty_msg(ui, tool);
                return;
            }
        };

        let mut changed = false;
        let mut drag_started = false;
        let mut desc = String::from("Edit");

        for g in &tp.groups {
            let sid = format!("live/{tool}/{}", g.label);
            let badge = tp.badge;
            f::section(ui, &sid, g.label, badge, |ui| {
                match g.label {
                    "Dimensions" => {
                        match &mut self.scene.primitives[idx].shape {
                            SdfShape::Box { half_extents: h, .. } => {
                                let mut v = [h.x, h.y, h.z];
                                let sig = f::vec3_row(ui, "Half extents", &mut v);
                                if sig.drag_started {
                                    drag_started = true;
                                }
                                if (v[0] - h.x).abs() + (v[1] - h.y).abs() + (v[2] - h.z).abs()
                                    > 1e-6
                                {
                                    h.x = v[0].max(0.001);
                                    h.y = v[1].max(0.001);
                                    h.z = v[2].max(0.001);
                                    changed = true;
                                    desc = "Resize".into();
                                }
                            }
                            SdfShape::Sphere { radius } => {
                                let mut r = *radius;
                                let resp = f::num(ui, "Radius", &mut r, 0.01, 0.0..=50.0, None);
                                if resp.drag_started() {
                                    drag_started = true;
                                }
                                if resp.changed() {
                                    *radius = r.max(0.001);
                                    changed = true;
                                    desc = "Resize".into();
                                }
                            }
                            SdfShape::Cylinder { radius, half_height } => {
                                let mut rad = *radius;
                                let mut hh = *half_height;
                                let r1 = f::num(ui, "Radius", &mut rad, 0.01, 0.0..=50.0, None);
                                let r2 =
                                    f::num(ui, "Half height", &mut hh, 0.01, 0.0..=50.0, None);
                                if r1.drag_started() || r2.drag_started() {
                                    drag_started = true;
                                }
                                if r1.changed() || r2.changed() {
                                    *radius = rad.max(0.001);
                                    *half_height = hh.max(0.001);
                                    changed = true;
                                    desc = "Resize".into();
                                }
                            }
                            SdfShape::Cone { radius, half_height } => {
                                let mut rad = *radius;
                                let mut hh = *half_height;
                                let r1 = f::num(ui, "Radius", &mut rad, 0.01, 0.0..=50.0, None);
                                let r2 =
                                    f::num(ui, "Half height", &mut hh, 0.01, 0.0..=50.0, None);
                                if r1.drag_started() || r2.drag_started() {
                                    drag_started = true;
                                }
                                if r1.changed() || r2.changed() {
                                    *radius = rad.max(0.001);
                                    *half_height = hh.max(0.001);
                                    changed = true;
                                    desc = "Resize".into();
                                }
                            }
                            SdfShape::Torus {
                                major_radius,
                                minor_radius,
                            } => {
                                let mut maj = *major_radius;
                                let mut minr = *minor_radius;
                                let r1 =
                                    f::num(ui, "Major radius", &mut maj, 0.01, 0.0..=50.0, None);
                                let r2 = f::num(
                                    ui,
                                    "Minor radius",
                                    &mut minr,
                                    0.01,
                                    0.001..=50.0,
                                    None,
                                );
                                if r1.drag_started() || r2.drag_started() {
                                    drag_started = true;
                                }
                                if r1.changed() || r2.changed() {
                                    *major_radius = maj.max(0.001);
                                    *minor_radius = minr.clamp(0.001, maj);
                                    changed = true;
                                    desc = "Resize".into();
                                }
                            }
                            SdfShape::Capsule { radius, half_height } => {
                                let mut rad = *radius;
                                let mut hh = *half_height;
                                let r1 = f::num(ui, "Radius", &mut rad, 0.01, 0.0..=50.0, None);
                                let r2 =
                                    f::num(ui, "Half height", &mut hh, 0.01, 0.0..=50.0, None);
                                if r1.drag_started() || r2.drag_started() {
                                    drag_started = true;
                                }
                                if r1.changed() || r2.changed() {
                                    *radius = rad.max(0.001);
                                    *half_height = hh.max(0.001);
                                    changed = true;
                                    desc = "Resize".into();
                                }
                            }
                            _ => {
                                for field in &g.fields {
                                    draw_field_temp(ui, tool, g.label, field);
                                }
                            }
                        }
                    }
                    "Corners" | "Edge Bevels" => {
                        // Full box edge-bevel suite (grouped + profiles + angles + advanced).
                        if let Some((c, d, de)) = self.draw_box_corners_params(ui, idx) {
                            if c {
                                changed = true;
                                desc = de;
                            }
                            if d {
                                drag_started = true;
                            }
                        }
                    }
                    "Boolean" => {
                        // Operation + Blend K + profile (Smooth / Chamfer / Curve).
                        // Profile UI mirrors Properties → SDF (owner: visible where Blend K lives).
                        //
                        // Operation goes through set_prims_boolean_op (single mutation path) so
                        // graph mirror + structural full-bake always fire — in-place mut of
                        // flat-cache alone left some sessions with Union still on the graph.
                        {
                            let uid = self.scene.primitives[idx].prim_uid;
                            let mut lab = op_label(&self.scene.primitives[idx].operation).to_owned();
                            let opts = ["Union", "Subtract", "Intersect", "Slice"];
                            // Unique id per prim — shared ("tp_combo","Operation") desynced when
                            // switching selection (combo showed old op / rejected click).
                            if f::enum_combo_id(
                                ui,
                                ("tp_op", uid),
                                "Operation",
                                &mut lab,
                                &opts,
                            ) {
                                let mut op = self.scene.primitives[idx].operation.clone();
                                set_op_kind(&mut op, &lab);
                                if self.set_prims_boolean_op(&[(uid, op)]) {
                                    // set_prims already undoes + rebakes; skip outer changed path.
                                    desc = "Boolean op".into();
                                }
                            }
                            let mut k = self.scene.primitives[idx].operation.blend_k();
                            let k_max = self.tweak.blend_k_max.max(0.1);
                            let resp = f::num(ui, "Blend K", &mut k, 0.01, 0.0..=k_max, None);
                            if resp.drag_started() {
                                drag_started = true;
                            }
                            if resp.changed() {
                                crate::edge_detect::op_set_k_clamped(
                                    &mut self.scene.primitives[idx].operation, k, k_max);
                                changed = true;
                                desc = "Blend K".into();
                            }
                        }
                        // Profile only when soft-blend is active (and not Seam/Extrude slice).
                        let show_profile = {
                            use core_math::primitives::BooleanOp;
                            let op = &self.scene.primitives[idx].operation;
                            let k = op.blend_k();
                            k > 0.0
                                && !matches!(
                                    op,
                                    BooleanOp::Seam { .. } | BooleanOp::Extrude { .. }
                                )
                        };
                        if show_profile {
                            use core_math::primitives::BlendProfile;
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new("Blend Profile")
                                    .small()
                                    .weak(),
                            )
                            .on_hover_text(
                                "Smooth = soft round · Chamfer = flat 45° bevel.",
                            );
                            ui.horizontal(|ui| {
                                let cur = self.scene.primitives[idx].blend_profile;
                                // Normalize legacy Curve/Circular display as Smooth.
                                let cur_ui = match cur {
                                    BlendProfile::Chamfer => BlendProfile::Chamfer,
                                    _ => BlendProfile::Polynomial,
                                };
                                for (p, lbl) in crate::properties_sdf::BLEND_PROFILE_CHOICES {
                                    if ui.selectable_label(cur_ui == p, lbl).clicked()
                                        && cur != p
                                    {
                                        self.scene.primitives[idx].blend_profile = p;
                                        changed = true;
                                        desc = "Blend profile".into();
                                    }
                                }
                            });
                        }
                    }
                    "Transform" => {
                        let p = &mut self.scene.primitives[idx];
                        let mut off = [p.position.x, p.position.y, p.position.z];
                        let mut rot = [p.rotation_deg.x, p.rotation_deg.y, p.rotation_deg.z];
                        let mut scl = [p.scale.x, p.scale.y, p.scale.z];
                        let s_off = f::vec3_row(ui, "Offset", &mut off);
                        let s_rot = f::vec3_row(ui, "Rotation", &mut rot);
                        let s_scl = f::vec3_row(ui, "Scale", &mut scl);
                        if s_off.drag_started || s_rot.drag_started || s_scl.drag_started {
                            drag_started = true;
                        }
                        let d_off = (off[0] - p.position.x).abs()
                            + (off[1] - p.position.y).abs()
                            + (off[2] - p.position.z).abs();
                        let d_rot = (rot[0] - p.rotation_deg.x).abs()
                            + (rot[1] - p.rotation_deg.y).abs()
                            + (rot[2] - p.rotation_deg.z).abs();
                        let d_scl = (scl[0] - p.scale.x).abs()
                            + (scl[1] - p.scale.y).abs()
                            + (scl[2] - p.scale.z).abs();
                        if d_off > 1e-6 {
                            p.position = glam::Vec3::new(off[0], off[1], off[2]);
                            changed = true;
                            desc = "Move".into();
                        }
                        if d_rot > 1e-6 {
                            p.rotation_deg = glam::Vec3::new(rot[0], rot[1], rot[2]);
                            changed = true;
                            desc = "Rotate".into();
                        }
                        if d_scl > 1e-6 {
                            p.scale = glam::Vec3::new(
                                scl[0].max(0.001),
                                scl[1].max(0.001),
                                scl[2].max(0.001),
                            );
                            changed = true;
                            desc = "Scale".into();
                        }
                    }
                    _ => {
                        for field in &g.fields {
                            draw_field_temp(ui, tool, g.label, field);
                        }
                    }
                }
            });
        }

        if changed {
            if drag_started && !self.has_active_episode() {
                self.begin_episode(&desc);
            } else if !drag_started && !self.has_active_episode() {
                self.transaction_snapshot_only(&desc);
            }
            self.mirror_prim_to_graph(idx);
            if self.is_master(idx) {
                let uid = self.scene.primitives[idx].prim_uid;
                self.sync_instances_of(uid);
            }
            self.rebake_no_undo();
        }
    }

    /// Full Box edge-bevel panel (Properties parity): up-axis, Top/Bot/Vert groups,
    /// per-group Chamfer, profiles, angles, Advanced 12-edge section.
    /// Returns `Some((changed, drag_started, desc))` when the prim is a Box.
    fn draw_box_corners_params(
        &mut self,
        ui: &mut egui::Ui,
        idx: usize,
    ) -> Option<(bool, bool, String)> {
        use core_math::primitives::{BevelMode, SdfShape};

        let SdfShape::Box { half_extents: h, bevel } = &self.scene.primitives[idx].shape else {
            return None;
        };
        let hx = h.x;
        let hy = h.y;
        let hz = h.z;
        let up_axis = bevel.up_axis;

        let (top_max, bot_max, vert_max) = {
            let (a, b, c) = match up_axis {
                1 => (hx, hy, hz),
                2 => (hz, hx, hy),
                _ => (hy, hx, hz),
            };
            let face_min = b.min(c);
            (face_min, face_min, face_min.min(a))
        };
        let (top_lbl, bot_lbl, vert_lbl) = match up_axis {
            1 => ("+X", "-X", "H"),
            2 => ("+Z", "-Z", "H"),
            _ => ("Top", "Bot", "Vert"),
        };

        let mut changed = false;
        let mut drag_started = false;
        let mut desc = String::from("Bevel");

        // Up axis
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Up axis")
                    .size(11.0)
                    .color(t::TEXT_DIM),
            );
            ui.add_space(6.0);
            for (ax, lbl) in [(0u8, "Y"), (1, "X"), (2, "Z")] {
                let on = if let SdfShape::Box { bevel, .. } = &self.scene.primitives[idx].shape {
                    bevel.up_axis == ax
                } else {
                    false
                };
                if ui.selectable_label(on, lbl).clicked() {
                    if let SdfShape::Box { bevel, .. } = &mut self.scene.primitives[idx].shape {
                        bevel.up_axis = ax;
                        changed = true;
                        desc = "Bevel axis".into();
                    }
                }
            }
        });

        // Grouped edge radii + per-group chamfer
        for (lbl, base, max_r) in [
            (top_lbl, 4usize, top_max),
            (bot_lbl, 0usize, bot_max),
            (vert_lbl, 8usize, vert_max),
        ] {
            let (mut group_r, mut ch_flag) =
                if let SdfShape::Box { bevel, .. } = &self.scene.primitives[idx].shape {
                    (bevel.radii[base], bevel.modes[base] == BevelMode::Chamfer)
                } else {
                    (0.0, false)
                };
            let r1 = f::num(ui, lbl, &mut group_r, 0.01, 0.0..=max_r.max(0.001), None);
            let r2 = f::bool_row(ui, &format!("{lbl} Chamfer"), &mut ch_flag);
            if r1.drag_started() {
                drag_started = true;
            }
            if r1.changed() {
                if let SdfShape::Box { bevel, .. } = &mut self.scene.primitives[idx].shape {
                    for ei in base..base + 4 {
                        bevel.radii[ei] = group_r;
                        if group_r > 0.0 && bevel.modes[ei] == BevelMode::None {
                            bevel.modes[ei] = BevelMode::Round;
                        }
                        if group_r == 0.0 {
                            bevel.modes[ei] = BevelMode::None;
                        }
                    }
                    changed = true;
                    desc = "Bevel".into();
                }
            }
            if r2.changed() {
                if let SdfShape::Box { bevel, .. } = &mut self.scene.primitives[idx].shape {
                    let m = if ch_flag {
                        BevelMode::Chamfer
                    } else {
                        BevelMode::Round
                    };
                    for ei in base..base + 4 {
                        bevel.modes[ei] = m;
                    }
                    changed = true;
                    desc = "Bevel mode".into();
                }
            }
        }

        // Profiles
        {
            let p = &mut self.scene.primitives[idx];
            let mut pt = p.bevel_profile_top;
            let mut pb = p.bevel_profile_bot;
            let mut pv = p.bevel_profile_vert;
            let r1 = f::num(ui, "Profile Top", &mut pt, 0.01, -1.0..=1.0, None);
            let r2 = f::num(ui, "Profile Bot", &mut pb, 0.01, -1.0..=1.0, None);
            let r3 = f::num(ui, "Profile Vert", &mut pv, 0.01, -1.0..=1.0, None);
            if r1.drag_started() || r2.drag_started() || r3.drag_started() {
                drag_started = true;
            }
            if r1.changed() || r2.changed() || r3.changed() {
                p.bevel_profile_top = pt;
                p.bevel_profile_bot = pb;
                p.bevel_profile_vert = pv;
                changed = true;
                desc = "Bevel profile".into();
            }
        }
        // Chamfer angles
        {
            let p = &mut self.scene.primitives[idx];
            let mut at = p.chamfer_angle_top;
            let mut ab = p.chamfer_angle_bot;
            let mut av = p.chamfer_angle_vert;
            let r1 = f::num(ui, "Angle Top", &mut at, 0.5, 1.0..=89.0, Some("°"));
            let r2 = f::num(ui, "Angle Bot", &mut ab, 0.5, 1.0..=89.0, Some("°"));
            let r3 = f::num(ui, "Angle Vert", &mut av, 0.5, 1.0..=89.0, Some("°"));
            if r1.drag_started() || r2.drag_started() || r3.drag_started() {
                drag_started = true;
            }
            if r1.changed() || r2.changed() || r3.changed() {
                p.chamfer_angle_top = at;
                p.chamfer_angle_bot = ab;
                p.chamfer_angle_vert = av;
                changed = true;
                desc = "Chamfer angle".into();
            }
        }

        // Advanced: 12 individual edges
        if let SdfShape::Box { bevel, .. } = &mut self.scene.primitives[idx].shape {
            egui::CollapsingHeader::new(
                egui::RichText::new("Advanced (per-edge)")
                    .size(11.0)
                    .color(t::TEXT_DIM),
            )
            .id_salt(("tp_box_bevel_edges", idx))
            .default_open(false)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(
                        "Each of the 12 edges independently. Overrides grouped sliders.",
                    )
                    .size(10.0)
                    .color(t::TEXT_FAINT),
                );
                let (top_grp, bot_grp) = match bevel.up_axis {
                    1 => ("+X face", "-X face"),
                    2 => ("+Z face", "-Z face"),
                    _ => ("Top face", "Bottom face"),
                };
                let edge_rows: [(&str, &str, usize, f32); 12] = [
                    (bot_grp, "near (a-)", 0, bot_max),
                    (bot_grp, "far (a+)", 1, bot_max),
                    (bot_grp, "left (b-)", 2, bot_max),
                    (bot_grp, "right (b+)", 3, bot_max),
                    (top_grp, "near (a-)", 4, top_max),
                    (top_grp, "far (a+)", 5, top_max),
                    (top_grp, "left (b-)", 6, top_max),
                    (top_grp, "right (b+)", 7, top_max),
                    ("Vertical", "b- a-", 8, vert_max),
                    ("Vertical", "b+ a-", 9, vert_max),
                    ("Vertical", "b- a+", 10, vert_max),
                    ("Vertical", "b+ a+", 11, vert_max),
                ];
                let mut last_grp = "";
                for (grp, lbl, ei, max_r) in edge_rows {
                    if grp != last_grp {
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new(grp)
                                .size(10.0)
                                .color(t::TEXT_SECTION)
                                .strong(),
                        );
                        last_grp = grp;
                    }
                    let mut r_val = bevel.radii[ei];
                    let mut ch_flag = bevel.modes[ei] == BevelMode::Chamfer;
                    let r1 = f::num(
                        ui,
                        lbl,
                        &mut r_val,
                        0.01,
                        0.0..=max_r.max(0.001),
                        None,
                    );
                    let r2 = f::bool_row(ui, "Chamfer", &mut ch_flag);
                    if r1.drag_started() {
                        drag_started = true;
                    }
                    if r1.changed() {
                        bevel.radii[ei] = r_val;
                        if r_val > 0.0 && bevel.modes[ei] == BevelMode::None {
                            bevel.modes[ei] = BevelMode::Round;
                        }
                        if r_val == 0.0 {
                            bevel.modes[ei] = BevelMode::None;
                        }
                        changed = true;
                        desc = "Bevel edge".into();
                    }
                    if r2.changed() {
                        bevel.modes[ei] = if ch_flag {
                            BevelMode::Chamfer
                        } else {
                            BevelMode::Round
                        };
                        changed = true;
                        desc = "Bevel edge mode".into();
                    }
                }
            });
        }

        Some((changed, drag_started, desc))
    }
}
