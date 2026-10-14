//! Modifier catalog + shared editor (pure-relocated from ui_panels.rs).

use eframe::egui;
use core_math::primitives::{ModifierType, SculptOp};

/// The ONE add-modifier catalog (display name → type) — the single source of truth
/// behind every add-modifier menu (properties combo, ui2 outliner context menu). `None` is
/// deliberately absent: it is the empty slot, not something a user adds.
pub(crate) const ADD_MODIFIER_TYPES: &[(&str, ModifierType)] = &[
    ("Mirror", ModifierType::Mirror), ("Twist", ModifierType::Twist),
    ("Taper", ModifierType::Taper), ("Smooth", ModifierType::Smooth),
    ("Displace", ModifierType::Displace), ("Shear", ModifierType::Shear),
    ("Sculpt", ModifierType::Sculpt), ("Spherize", ModifierType::Spherize),
    ("Shell", ModifierType::Shell), ("Emboss", ModifierType::Emboss),
    ("Deboss", ModifierType::Deboss), ("Array", ModifierType::Array),
];

/// Fresh `Modifier` with the per-type workable defaults — the ONE source for every
/// modifier picker (properties "+ Add Modifier" combo, X boolean pie Emboss/Deboss).
pub(crate) fn default_modifier(mt: ModifierType) -> core_math::primitives::Modifier {
    let mut m = core_math::primitives::Modifier::default();
    m.mod_type = mt;
    match mt {
        ModifierType::Mirror => { m.axis = 1; }
        ModifierType::Displace => { m.strength = 0.1; m.param2 = 5.0; }
        ModifierType::Twist => { m.strength = 1.0; }
        ModifierType::Taper => { m.strength = 0.5; m.param2 = 0.3; }
        ModifierType::Shear => { m.strength = 0.3; }
        ModifierType::Spherize => { m.strength = 1.0; m.param2 = 0.5; }
        ModifierType::Shell => { m.strength = 0.1; m.param2 = 0.0; }
        ModifierType::Sculpt => {
            m.strength = 0.2; m.param2 = 0.0; // Push
            m.mask.shape = core_math::primitives::MaskShape::Sphere;
            m.mask.extents = glam::Vec3::splat(0.5); m.mask.feather = 0.4;
        }
        // Emboss/Deboss: Sphere stamp (dome relief) + moderate amplitude; sign fixed by the variant.
        ModifierType::Emboss | ModifierType::Deboss => {
            m.strength = 0.15;
            m.mask.shape = core_math::primitives::MaskShape::Sphere;
            m.mask.extents = glam::Vec3::splat(0.4); m.mask.feather = 0.4;
        }
        ModifierType::Array => {
            m.strength = 3.0; // count
            m.param2 = 0.0; // linear by default (≥0.5 = circular)
            m.mask.center = glam::Vec3::new(1.5, 0.0, 0.0); // linear step / circle radius vec
        }
        _ => {}
    }
    m
}

/// Shared modifier type/strength/axis editor. Returns (changed, drag_started).
pub(crate) fn draw_modifier_editor(ui: &mut egui::Ui, modifier: &mut core_math::primitives::Modifier, id_salt: &str) -> (bool, bool) {
    let mut changed = false;
    let mut drag_started = false;
    let mod_types = [("None", ModifierType::None), ("Twist", ModifierType::Twist),
                     ("Taper", ModifierType::Taper), ("Smooth", ModifierType::Smooth),
                     ("Displace", ModifierType::Displace), ("Shear", ModifierType::Shear),
                     ("Sculpt", ModifierType::Sculpt), ("Spherize", ModifierType::Spherize),
                     ("Shell", ModifierType::Shell), ("Emboss", ModifierType::Emboss),
                     ("Deboss", ModifierType::Deboss), ("Array", ModifierType::Array)];
    let cur_label = mod_types.iter().find(|t| t.1 == modifier.mod_type).map(|t| t.0).unwrap_or("None");
    egui::ComboBox::from_id_salt(format!("{}_mod_type", id_salt)).selected_text(cur_label).show_ui(ui, |ui| {
        for (name, mt) in &mod_types {
            if ui.selectable_label(modifier.mod_type == *mt, *name).clicked() {
                modifier.mod_type = *mt;
                changed = true;
            }
        }
    });
    if modifier.mod_type != ModifierType::None {
        // Strength slider — label/range depend on the modifier (morph 0..1, thickness 0..1, else ±3).
        let (s_lbl, s_lo, s_hi) = match modifier.mod_type {
            ModifierType::Spherize => ("Morph", 0.0, 1.0),
            ModifierType::Shell => ("Thickness", 0.0, 1.0),
            // Emboss/Deboss relief amplitude — sign is fixed by the variant, so a positive-only slider.
            ModifierType::Emboss | ModifierType::Deboss => ("Relief", 0.0, 1.0),
            _ => ("Strength", -3.0, 3.0),
        };
        let r = ui.add(egui::Slider::new(&mut modifier.strength, s_lo..=s_hi).text(s_lbl).step_by(0.01));
        if r.drag_started() { drag_started = true; }
        if r.changed() { changed = true; }
        let axis = &mut modifier.axis;
        egui::ComboBox::from_id_salt(format!("{}_axis", id_salt))
            .selected_text(match *axis { 0 => "Y", 1 => "X", _ => "Z" })
            .show_ui(ui, |ui| {
                if ui.selectable_value(axis, 0, "Y").changed() { changed = true; }
                if ui.selectable_value(axis, 1, "X").changed() { changed = true; }
                if ui.selectable_value(axis, 2, "Z").changed() { changed = true; }
            });
        if modifier.mod_type == ModifierType::Taper {
            let r = ui.add(egui::Slider::new(&mut modifier.param2, 0.01..=3.0).text("End Scale"));
            if r.drag_started() { drag_started = true; }
            if r.changed() { changed = true; }
        }
        if modifier.mod_type == ModifierType::Displace {
            let r = ui.add(egui::Slider::new(&mut modifier.param2, 0.1..=20.0).text(
                if modifier.height_map.is_some() { "UV Scale" } else { "Frequency" },
            ));
            if r.drag_started() { drag_started = true; }
            if r.changed() { changed = true; }
            if modifier.height_map.is_some() {
                ui.label(
                    egui::RichText::new("Image height drives displacement (noise off)")
                        .small()
                        .weak()
                        .italics(),
                );
            }
        }
        if modifier.mod_type == ModifierType::Sculpt {
            // Sculpt operation selector
            let cur_op = SculptOp::from_f32(modifier.param2);
            egui::ComboBox::from_id_salt(format!("{}_sculpt_op", id_salt))
                .selected_text(cur_op.label())
                .show_ui(ui, |ui| {
                    for op in &SculptOp::ALL {
                        if ui.selectable_label(*op == cur_op, op.label()).clicked() {
                            modifier.param2 = op.to_f32();
                            changed = true;
                        }
                    }
                });
            // Hint text for selected operation
            let hint = match cur_op {
                SculptOp::Push => "Grow surface outward (add material)",
                SculptOp::Pull => "Shrink surface inward (remove material)",
                SculptOp::Flatten => "Level surface toward mask center plane",
                SculptOp::Inflate => "Expand uniformly (stronger near surface)",
                SculptOp::Pinch => "Contract toward the axis line",
                SculptOp::Crease => "Create groove along axis direction",
            };
            ui.label(egui::RichText::new(hint).weak().italics().size(10.0));
        }
        if modifier.mod_type == ModifierType::Shear {
            let drive = modifier.param2 as u8;
            egui::ComboBox::from_id_salt(format!("{}_drive", id_salt))
                .selected_text(match drive { 1 => "X", 2 => "Z", _ => "Y" })
                .show_ui(ui, |ui| {
                    if ui.selectable_label(drive == 0, "Y").clicked() { modifier.param2 = 0.0; changed = true; }
                    if ui.selectable_label(drive == 1, "X").clicked() { modifier.param2 = 1.0; changed = true; }
                    if ui.selectable_label(drive == 2, "Z").clicked() { modifier.param2 = 2.0; changed = true; }
                });
        }
        if modifier.mod_type == ModifierType::Spherize {
            // param2 = target sphere radius (local units); 0 falls back to 0.5 at eval time.
            let r = ui.add(egui::Slider::new(&mut modifier.param2, 0.05..=2.0).text("Radius"));
            if r.drag_started() { drag_started = true; }
            if r.changed() { changed = true; }
            ui.label(egui::RichText::new("Morphs the shape toward a sphere of this radius").weak().italics().size(10.0));
        }
        if modifier.mod_type == ModifierType::Shell {
            ui.label(egui::RichText::new("Hollows the surface (wall = 2× thickness)").weak().italics().size(10.0));
        }
    }
    (changed, drag_started)
}
