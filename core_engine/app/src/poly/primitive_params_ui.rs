//! The ONE parameter widget for a parametric poly primitive.
//!
//! Plan: `docs/planning/poly/PARAMETRIC_PRIMITIVES.md` §2.4.
//!
//! Both the F9 "adjust last create" redo panel (`tool_params.rs`) and — from Gate 2 — the
//! properties-panel primitive block draw their sliders from here. Before this existed the same
//! parameters were spelled out in three places (`PolyCreateAdjust`'s flat fields, the redo panel's
//! slider block, and the static `poly_primitive()` catalog) with nothing forcing them to agree.

use core_math::primitive_spec::{
    ArcClose, ArcSpec, CapFill, PrimitiveSpec, RoundSpec, MAX_ICO_SUBDIV, MAX_SIDES, MAX_SUBDIV,
};

/// What a parameter pass produced.
#[derive(Default, Clone, Copy)]
pub(crate) struct SpecEdit {
    /// A parameter changed this frame — the caller must rebuild the mesh.
    pub(crate) changed: bool,
    /// A drag/edit finished — the caller must commit ONE undo entry.
    pub(crate) released: bool,
}

impl SpecEdit {
    fn fold(&mut self, r: &egui::Response) {
        self.changed |= r.changed();
        self.released |= r.drag_stopped() || r.lost_focus();
    }
}

/// Draw every parameter for `spec`. Returns what changed.
///
/// Slider bounds mirror the clamp domain in `PrimitiveSpec::build()` so the user never watches a
/// value silently snap after release.
pub(crate) fn draw_primitive_spec_params(ui: &mut egui::Ui, spec: &mut PrimitiveSpec) -> SpecEdit {
    let mut e = SpecEdit::default();
    let max_sides = MAX_SIDES as i64;
    let max_sub = MAX_SUBDIV as i64;

    match spec {
        PrimitiveSpec::Box(s) => {
            ui.label(dim(ui, "Size"));
            for (lab, v) in [
                ("X", &mut s.size.x),
                ("Y", &mut s.size.y),
                ("Z", &mut s.size.z),
            ] {
                e.fold(&ui.add(egui::Slider::new(v, 0.01..=20.0).text(lab)));
            }
            ui.label(dim(ui, "Segments"));
            for (lab, v) in [
                ("X", &mut s.seg_x),
                ("Y", &mut s.seg_y),
                ("Z", &mut s.seg_z),
            ] {
                e.fold(&ui.add(egui::Slider::new(v, 1..=max_sub as u32).text(lab)));
            }
            e.changed |= round_params(ui, &mut e, &mut s.round);
        }
        PrimitiveSpec::Plane(s) => {
            e.fold(&ui.add(egui::Slider::new(&mut s.width, 0.01..=40.0).text("Width")));
            e.fold(&ui.add(egui::Slider::new(&mut s.depth, 0.01..=40.0).text("Depth")));
            e.fold(&ui.add(egui::Slider::new(&mut s.subdivs_w, 1..=max_sub as u32).text("Subdiv W")));
            e.fold(&ui.add(egui::Slider::new(&mut s.subdivs_d, 1..=max_sub as u32).text("Subdiv D")));
        }
        PrimitiveSpec::Circle(s) => {
            e.fold(&ui.add(egui::Slider::new(&mut s.radius, 0.01..=20.0).text("Radius")));
            e.fold(&ui.add(egui::Slider::new(&mut s.sides, 3..=max_sides as u32).text("Sides")));
            e.changed |= cap_fill_combo(ui, "Fill", &mut s.fill, "circle");
            e.changed |= arc_params(ui, &mut e, &mut s.arc, "circle");
        }
        PrimitiveSpec::UvSphere(s) => {
            e.fold(&ui.add(egui::Slider::new(&mut s.radius, 0.01..=20.0).text("Radius")));
            e.fold(&ui.add(egui::Slider::new(&mut s.segments, 3..=max_sides as u32).text("Segments")));
            e.fold(&ui.add(egui::Slider::new(&mut s.rings, 2..=max_sub as u32).text("Rings")));
            e.changed |= arc_params(ui, &mut e, &mut s.arc, "uvsphere");
        }
        PrimitiveSpec::IcoSphere(s) => {
            e.fold(&ui.add(egui::Slider::new(&mut s.radius, 0.01..=20.0).text("Radius")));
            e.fold(&ui.add(
                egui::Slider::new(&mut s.subdivisions, 0..=MAX_ICO_SUBDIV).text("Subdivisions"),
            ));
            if s.subdivisions >= 4 {
                ui.label(warn(
                    ui,
                    &format!("{} triangles", 20 * 4usize.pow(s.subdivisions)),
                ));
            }
        }
        PrimitiveSpec::Cylinder(s) => {
            e.fold(&ui.add(egui::Slider::new(&mut s.radius, 0.01..=20.0).text("Radius")));
            e.fold(&ui.add(egui::Slider::new(&mut s.height, 0.01..=40.0).text("Height")));
            e.fold(&ui.add(egui::Slider::new(&mut s.sides, 3..=max_sides as u32).text("Sides")));
            e.fold(&ui.add(
                egui::Slider::new(&mut s.vsegments, 1..=max_sub as u32).text("V Segments"),
            ));
            e.changed |= cap_fill_combo(ui, "Cap Fill", &mut s.cap_fill, "cyl");
            e.changed |= arc_params(ui, &mut e, &mut s.arc, "cyl");
            e.changed |= round_params(ui, &mut e, &mut s.round);
        }
        PrimitiveSpec::Cone(s) => {
            e.fold(&ui.add(
                egui::Slider::new(&mut s.radius_bottom, 0.01..=20.0).text("Radius Bottom"),
            ));
            let r = ui.add(egui::Slider::new(&mut s.radius_top, 0.0..=20.0).text("Radius Top"));
            if r.hovered() {
                r.clone().on_hover_text("Above 0 turns the cone into a truncated cone (frustum)");
            }
            e.fold(&r);
            e.fold(&ui.add(egui::Slider::new(&mut s.height, 0.01..=40.0).text("Height")));
            e.fold(&ui.add(egui::Slider::new(&mut s.sides, 3..=max_sides as u32).text("Sides")));
            e.fold(&ui.add(
                egui::Slider::new(&mut s.vsegments, 1..=max_sub as u32).text("V Segments"),
            ));
            e.changed |= cap_fill_combo(ui, "Cap Fill", &mut s.cap_fill, "cone");
            e.changed |= arc_params(ui, &mut e, &mut s.arc, "cone");
        }
        PrimitiveSpec::Torus(s) => {
            e.fold(&ui.add(egui::Slider::new(&mut s.major_r, 0.01..=20.0).text("Radius")));
            // Bounded by the major radius: past it the tube self-intersects through the hub.
            let minor_max = s.major_r.max(0.02);
            e.fold(&ui.add(
                egui::Slider::new(&mut s.minor_r, 0.01..=minor_max).text("Minor Radius"),
            ));
            e.fold(&ui.add(
                egui::Slider::new(&mut s.major_segs, 3..=max_sides as u32).text("Major Segments"),
            ));
            e.fold(&ui.add(
                egui::Slider::new(&mut s.minor_segs, 3..=max_sides as u32).text("Minor Segments"),
            ));
            e.changed |= arc_params(ui, &mut e, &mut s.arc, "torus");
        }
    }
    e
}

/// Sweep controls — "remove a slice of the pie". Shared by every revolved primitive.
fn arc_params(ui: &mut egui::Ui, e: &mut SpecEdit, arc: &mut ArcSpec, salt: &str) -> bool {
    let mut changed = false;
    ui.separator();
    ui.label(dim(ui, "Arc"));
    let r = ui.add(
        egui::Slider::new(&mut arc.sweep_deg, ArcSpec::MIN_SWEEP..=360.0)
            .suffix("°")
            .text("Sweep"),
    );
    e.fold(&r);
    let r = ui.add(
        egui::Slider::new(&mut arc.start_deg, -360.0..=360.0)
            .suffix("°")
            .text("Start"),
    );
    e.fold(&r);

    // `close` is meaningless on a full revolution — the seam welds. Grey it out rather than
    // letting the user set a control that silently does nothing.
    ui.add_enabled_ui(!arc.is_full(), |ui| {
        egui::ComboBox::from_id_salt(format!("arcclose/{salt}"))
            .selected_text(match arc.close {
                ArcClose::Wall => "Wall",
                ArcClose::Open => "Open",
            })
            .show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(&mut arc.close, ArcClose::Wall, "Wall")
                    .clicked();
                changed |= ui
                    .selectable_value(&mut arc.close, ArcClose::Open, "Open")
                    .clicked();
            });
        ui.label(dim(ui, "Ends"));
    });
    if changed {
        e.released = true;
    }
    changed
}

fn cap_fill_combo(ui: &mut egui::Ui, label: &str, fill: &mut CapFill, salt: &str) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt(format!("capfill/{salt}"))
        .selected_text(match fill {
            CapFill::None => "None",
            CapFill::NGon => "N-Gon",
            CapFill::TriFan => "Triangle Fan",
        })
        .show_ui(ui, |ui| {
            changed |= ui.selectable_value(fill, CapFill::NGon, "N-Gon").clicked();
            changed |= ui
                .selectable_value(fill, CapFill::TriFan, "Triangle Fan")
                .clicked();
            changed |= ui.selectable_value(fill, CapFill::None, "None").clicked();
        });
    ui.label(dim(ui, label));
    changed
}

/// Rounded corners / rim.
fn round_params(ui: &mut egui::Ui, e: &mut SpecEdit, round: &mut RoundSpec) -> bool {
    ui.separator();
    ui.label(dim(ui, "Round"));
    let r = ui.add(egui::Slider::new(&mut round.radius, 0.0..=2.0).text("Radius"));
    e.fold(&r);
    ui.add_enabled_ui(!round.is_off(), |ui| {
        let r = ui.add(
            egui::Slider::new(&mut round.segments, 1..=RoundSpec::MAX_SEGMENTS).text("Segments"),
        );
        e.fold(&r);
    });
    false
}

fn dim(_ui: &egui::Ui, s: &str) -> egui::RichText {
    egui::RichText::new(s)
        .size(10.5)
        .color(crate::ui_theme::TEXT_DIM)
}

fn warn(_ui: &egui::Ui, s: &str) -> egui::RichText {
    egui::RichText::new(s)
        .size(10.0)
        .color(crate::ui_theme::TEXT_FAINT)
}
