//! Outliner context menus and eye menu.
use crate::ui_menus::AzraMenus;
use eframe::egui;
use egui_phosphor::regular as ph;
use crate::*;
use super::types::*;

/// `.ctxmenu button` — `icon  Label   value`, with the current value trailing in faint mono so
/// a submenu shows what it is set to WITHOUT being opened (the artifact's `trail()`).
pub(crate) fn ol_menu_label(icon: &str, label: &str, value: &str, tint: OlTint) -> egui::text::LayoutJob {
    let fmt = |size: f32, mono: bool, color: egui::Color32| egui::TextFormat {
        font_id: if mono { egui::FontId::monospace(size) } else { egui::FontId::proportional(size) },
        color,
        valign: egui::Align::Center,
        ..Default::default()
    };
    let (ic, tc) = match tint {
        OlTint::Normal => (ui_theme::TEXT_DIM, ui_theme::TEXT_SECONDARY),
        OlTint::Icon(c) => (c, ui_theme::TEXT_SECONDARY),
        OlTint::Danger => (ui_theme::DESTRUCTIVE, ui_theme::DESTRUCTIVE),
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(icon, 0.0, fmt(15.0, false, ic));
    job.append(label, 9.0, fmt(12.0, false, tc));
    if !value.is_empty() { job.append(value, 8.0, fmt(11.0, true, ui_theme::TEXT_FAINT)); }
    job
}

/// `.ctxmenu .seclabel` — 9.5px uppercase section header naming the row's type.
pub(crate) fn ol_menu_section(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text.to_uppercase()).size(9.5).color(ui_theme::TEXT_SECTION));
}

/// One row's context menu. Reads ONLY `row` — every outcome leaves through the out-params, so
/// nothing here can touch the scene while the list is being painted.
/// Visibility / lock route into the SAME out-params the eye and lock chip use: one path per
/// mutation, whether it was reached by clicking the icon or by picking the menu item.
pub(crate) fn ol_context_menu(ui: &mut egui::Ui, row: &OlRow, keys: &OlKeys,
                   act: &mut Option<OlAct>,
                   toggle_vis: &mut Option<(OlItem, bool)>,
                   toggle_lock: &mut Option<(OlItem, bool)>) {
    ui.set_min_width(178.0);
    match (&row.menu, row.item) {
        (OlMenu::Prim { op, shape, is_base, instance_master }, OlItem::Prim(uid)) => {
            ol_menu_section(ui, "SDF Primitive");
            if *is_base {
                // A body's BASE operand is not booleaned against anything — offering ops here
                // would silently do nothing. v1 shows the same note (outliner.rs:234).
                ui.add_enabled(false, egui::Label::new(
                    egui::RichText::new("Base — no boolean op").size(11.0).color(ui_theme::TEXT_FAINT)));
            } else {
                // Seam/Extrude have no badge (they displace rather than add/cut volume), so
                // they fall back to a neutral glyph instead of borrowing another op's hue.
                let (bc, bi) = op_visual(op).unwrap_or((ui_theme::TEXT_DIM, ph::CIRCLE_HALF));
                ui.azra_menu_button(ol_menu_label(bi, "Boolean", ol_bool_op_name(op), OlTint::Icon(bc)), |ui| {
                    // The shared 9-item builder (hard/smooth Union+Subtract, Intersect, Seam,
                    // Extrude) — `true` selects the outliner variant. It closes its own menu.
                    if let Some(new_op) = crate::ui_panels::bool_op_menu_items(ui, op, true) {
                        *act = Some(OlAct::SetBool(uid, new_op));
                    }
                });
            }
            ui.azra_menu_button(ol_menu_label(ph::CUBE, "Change Shape", shape, OlTint::Normal), |ui| {
                let mut presets: Vec<core_math::primitives::SdfShape> =
                    OL_SHAPE_PRESETS.iter().map(|&i| FourthLayerApp::shape_preset(i)).collect();
                presets.push(FourthLayerApp::curve_preset());
                presets.push(core_math::primitives::SdfShape::Billboard {
                    width: 1.0, height: 1.0, face_camera: false });
                for s in presets {
                    let label = s.label().to_string();
                    let is_cur = label == *shape;
                    if ui.add(egui::SelectableLabel::new(is_cur,
                        ol_menu_label(prim_shape_icon(&s), &label, "", OlTint::Normal))).clicked() {
                        if !is_cur { *act = Some(OlAct::SetShape(uid, s)); }
                        ui.close_menu();
                    }
                }
            });
            ui.azra_menu_button(ol_menu_label(ph::WRENCH, "Add Modifier", "", OlTint::Normal), |ui| {
                for (name, mt) in crate::ui_panels::ADD_MODIFIER_TYPES {
                    if ui.button(*name).clicked() { *act = Some(OlAct::AddMod(uid, *mt)); ui.close_menu(); }
                }
            });
            ui.separator();
            // Instance ops (v1 outliner prim menu parity).
            ol_menu_section(ui, "Instance");
            if let Some(master) = *instance_master {
                if ui
                    .button(ol_menu_label(ph::LINK, "Make Unique / Break Link", "", OlTint::Normal))
                    .on_hover_text("Instance → independent (stop following master)")
                    .clicked()
                {
                    *act = Some(OlAct::MakeUnique(uid));
                    ui.close_menu();
                }
                if ui
                    .button(ol_menu_label(ph::STAR, "Select Master", "", OlTint::Normal))
                    .clicked()
                {
                    *act = Some(OlAct::SelectMaster(master));
                    ui.close_menu();
                }
            } else {
                if ui
                    .button(ol_menu_label(ph::COPY, "Create Instance", "", OlTint::Normal))
                    .clicked()
                {
                    *act = Some(OlAct::CreateInstance(uid));
                    ui.close_menu();
                }
                if ui
                    .button(ol_menu_label(ph::STACK, "Instance Into All Bodies", "", OlTint::Normal))
                    .on_hover_text(
                        "Linked copy of this prim in every other top-level body \
                         (same shape/boolean; edit master to update all).",
                    )
                    .clicked()
                {
                    *act = Some(OlAct::InstanceIntoAll(uid));
                    ui.close_menu();
                }
            }
            ui.separator();
            // Prims-only: `group_selected_into_body` operates on the selected PRIMS and needs
            // them to share one body. `None` = menu/keyboard semantics — `Some(uid)` is the
            // drag-onto-prim form, where the new group would steal the target's boolean op.
            if ui.button(ol_menu_label(ph::FOLDER_PLUS, "Group into Sub-body", "", OlTint::Normal)).clicked() {
                *act = Some(OlAct::Group); ui.close_menu();
            }
            ui.separator();
        }
        (OlMenu::SdfBody { is_root_body }, OlItem::Node(n)) => {
            let nid = NodeId(n);
            ol_menu_section(ui, "SDF Body");
            if !row.active_body {
                if ui.button(ol_menu_label(ph::TARGET, "Set Active Body", "", OlTint::Normal)).clicked() {
                    *act = Some(OlAct::SetActiveBody(nid)); ui.close_menu();
                }
            }
            if ui.button(ol_menu_label(ph::FLOPPY_DISK, "Save as Part", "", OlTint::Normal)).clicked() {
                *act = Some(OlAct::SaveAsPart(nid)); ui.close_menu();
            }
            if ui.button(ol_menu_label(ph::CUBE, "Convert to Mesh", "", OlTint::Normal)).clicked() {
                *act = Some(OlAct::ConvertToPoly(nid)); ui.close_menu();
            }
            if !row.active_body {
                if ui.button(ol_menu_label(ph::SIGN_IN, "Enter Scope", "", OlTint::Normal)).clicked() {
                    *act = Some(OlAct::EnterScope(nid)); ui.close_menu();
                }
            }
            // The root body has nowhere to dissolve INTO — the backend refuses it with a toast,
            // so the item is hidden rather than shown as a guaranteed failure.
            if !*is_root_body {
                if ui.button(ol_menu_label(ph::FOLDER_MINUS, "Ungroup (Dissolve)", "", OlTint::Normal)).clicked() {
                    *act = Some(OlAct::Dissolve(nid)); ui.close_menu();
                }
            }
            ui.separator();
        }
        (OlMenu::Poly, OlItem::Node(n)) => {
            let nid = NodeId(n);
            ol_menu_section(ui, "Polygon Mesh");
            if ui
                .button(ol_menu_label(ph::PAINT_BRUSH, "Surface Sculpt", "", OlTint::Normal))
                .on_hover_text(
                    "3D-Coat Surface: sculpt this mesh with the sculpt-room brushes.\n\
                     Writes the mesh — no Sculpt modifier. Add a Sculpt modifier only for a non-destructive layer.",
                )
                .clicked()
            {
                *act = Some(OlAct::SurfaceSculpt(nid));
                ui.close_menu();
            }
            if ui
                .button(ol_menu_label(ph::FLOPPY_DISK, "Save to Part…", "", OlTint::Normal))
                .on_hover_text(
                    "Save this poly mesh to the kitbash library as a reusable part.\n\
                     Thumbnail: gray matcap, square crop zoomed on the mesh, transparent background.",
                )
                .clicked()
            {
                *act = Some(OlAct::SaveAsPart(nid));
                ui.close_menu();
            }
            if ui
                .button(ol_menu_label(ph::GRID_FOUR, "Subdivide (CC)", "", OlTint::Normal))
                .clicked()
            {
                *act = Some(OlAct::Subdivide(nid));
                ui.close_menu();
            }
            if ui
                .button(ol_menu_label(ph::CIRCLES_THREE, "Convert to SDF Primitive", "", OlTint::Normal))
                .on_hover_text(
                    "Convert this closed poly into a MeshVolume SDF primitive in the active body \
                     (poly consumed)",
                )
                .clicked()
            {
                *act = Some(OlAct::ConvertToSdf(nid));
                ui.close_menu();
            }
            if ui
                .button(ol_menu_label(ph::LINK, "Link as SDF Modifier", "", OlTint::Normal))
                .on_hover_text(
                    "LIVE: the poly becomes an SDF primitive that follows this mesh. \
                     The poly is hidden (not deleted).",
                )
                .clicked()
            {
                *act = Some(OlAct::LinkAsSdf(nid));
                ui.close_menu();
            }
            if ui
                .button(ol_menu_label(ph::DOWNLOAD_SIMPLE, "Export OBJ…", "", OlTint::Normal))
                .clicked()
            {
                *act = Some(OlAct::ExportObj(nid));
                ui.close_menu();
            }
            if ui
                .button(ol_menu_label(ph::IMAGE, "Bake Mesh Maps…", "", OlTint::Normal))
                .on_hover_text(
                    "Bake Curvature / AO / Normal / Position into the UV unwrap.\n\
                     Run Unwrap (Smart) first if this body has no UVs.",
                )
                .clicked()
            {
                *act = Some(OlAct::BakeMeshMaps(nid));
                ui.close_menu();
            }
            ui.separator();
        }
        (OlMenu::Voxel, OlItem::Node(n)) => {
            let nid = NodeId(n);
            ol_menu_section(ui, "Voxel Object");
            if ui
                .button(ol_menu_label(ph::PAINT_BRUSH, "Surface Sculpt", "", OlTint::Normal))
                .on_hover_text(
                    "3D-Coat Surface: switch this voxel to mesh sculpt.\n\
                     Brushes displace the surface. Toggle Voxel in the sculpt panel to revoxelize.",
                )
                .clicked()
            {
                *act = Some(OlAct::SurfaceSculpt(nid));
                ui.close_menu();
            }
            ui.separator();
        }
        (OlMenu::Convertible, OlItem::Node(n)) => {
            let nid = NodeId(n);
            ol_menu_section(ui, "Mesh");
            if ui.button(ol_menu_label(ph::CUBE, "Convert to Editable Poly", "", OlTint::Normal)).clicked() {
                *act = Some(OlAct::ConvertToPoly(nid)); ui.close_menu();
            }
            if ui
                .button(ol_menu_label(ph::DOWNLOAD_SIMPLE, "Export OBJ…", "", OlTint::Normal))
                .clicked()
            {
                *act = Some(OlAct::ExportObj(nid));
                ui.close_menu();
            }
            ui.separator();
        }
        (OlMenu::Group, OlItem::Node(n)) => {
            let nid = NodeId(n);
            ol_menu_section(ui, "Group");
            if ui
                .button(ol_menu_label(ph::FLOPPY_DISK, "Save Group to Library…", "", OlTint::Normal))
                .on_hover_text("Save this group + all child bodies as one kitbash part")
                .clicked()
            {
                *act = Some(OlAct::SaveAsPart(nid));
                ui.close_menu();
            }
            ui.separator();
        }
        (OlMenu::Light { id, light_type }, _) => {
            ol_menu_section(ui, "Light");
            let cur = *light_type;
            let lid = *id;
            ui.azra_menu_button(ol_menu_label(ph::LIGHTBULB, "Light Type", match cur {
                core_math::scene_graph::LightType::Directional => "Directional",
                core_math::scene_graph::LightType::Point => "Point",
                core_math::scene_graph::LightType::Spot => "Spot",
            }, OlTint::Normal), |ui| {
                for (lt, label) in [
                    (core_math::scene_graph::LightType::Directional, "Directional"),
                    (core_math::scene_graph::LightType::Point, "Point"),
                    (core_math::scene_graph::LightType::Spot, "Spot"),
                ] {
                    if ui.add(egui::SelectableLabel::new(cur == lt, label)).clicked() {
                        if cur != lt {
                            *act = Some(OlAct::SetLightType(lid, lt));
                        }
                        ui.close_menu();
                    }
                }
            });
            ui.separator();
        }
        (OlMenu::Material { mid }, OlItem::Node(n)) => {
            let nid = NodeId(n);
            let mat_id = *mid;
            ol_menu_section(ui, "Material");
            if ui
                .button(ol_menu_label(ph::PAINT_BRUSH, "Assign to Selected", "", OlTint::Normal))
                .on_hover_text(
                    "Assign this material to the current selection \
                     (SDF prims / body, or poly faces / object).",
                )
                .clicked()
            {
                *act = Some(OlAct::AssignMaterialToSelection(mat_id));
                ui.close_menu();
            }
            if ui
                .button(ol_menu_label(ph::SELECTION, "Select by Material", "", OlTint::Normal))
                .on_hover_text(
                    "Select every poly face that uses this material \
                     (enters Face mode). Also selects SDF prims that reference it.",
                )
                .clicked()
            {
                *act = Some(OlAct::SelectByMaterial(mat_id));
                ui.close_menu();
            }
            ui.separator();
            if ui.button(ol_menu_label(ph::COPY, "Duplicate Material", "", OlTint::Normal)).clicked() {
                *act = Some(OlAct::DuplicateMaterial(nid));
                ui.close_menu();
            }
            if ui
                .button(ol_menu_label(ph::TRASH, "Delete Material", "", OlTint::Danger))
                .on_hover_text("In-use refs revert to default material (undoable)")
                .clicked()
            {
                *act = Some(OlAct::DeleteMaterial(nid));
                ui.close_menu();
            }
            ui.separator();
            // Material path: rename + frame only (no lock / hide / hierarchy nest).
            if ui.button(ol_menu_label(ph::PENCIL_SIMPLE, "Rename", &keys.rename, OlTint::Normal)).clicked() {
                if let Some(t) = row.rename {
                    *act = Some(OlAct::StartRename(t, row.name.clone()));
                }
                ui.close_menu();
            }
            if ui.button(ol_menu_label(ph::CROSSHAIR, "Frame Selected", &keys.focus, OlTint::Normal)).clicked() {
                *act = Some(OlAct::Frame);
                ui.close_menu();
            }
            return;
        }
        (OlMenu::MatMap { parent, kind }, _) => {
            ol_menu_section(ui, "Map Channel");
            let now = ui.input(|i| i.time);
            if ui
                .button(ol_menu_label(ph::PENCIL_SIMPLE, "Edit Channel", "", OlTint::Normal))
                .clicked()
            {
                *act = Some(OlAct::FocusMapChannel(*parent, *kind, now));
                ui.close_menu();
            }
            if ui
                .button(ol_menu_label(ph::TRASH, "Clear Map", "", OlTint::Danger))
                .on_hover_text("Unlink this map from the channel (undoable)")
                .clicked()
            {
                *act = Some(OlAct::ClearMap(*parent, *kind));
                ui.close_menu();
            }
            return;
        }
        _ => {}
    }

    // —— Common tail (the artifact's fixed block) ——
    if ui.button(ol_menu_label(ph::PENCIL_SIMPLE, "Rename", &keys.rename, OlTint::Normal)).clicked() {
        if let Some(t) = row.rename { *act = Some(OlAct::StartRename(t, row.name.clone())); }
        ui.close_menu();
    }
    if ui.button(ol_menu_label(ph::COPY, "Duplicate", &keys.dup, OlTint::Normal)).clicked() {
        *act = Some(OlAct::Duplicate); ui.close_menu();
    }
    let (lk_icon, lk_text) = if row.locked { (ph::LOCK_SIMPLE_OPEN, "Unlock") } else { (ph::LOCK_SIMPLE, "Lock") };
    if ui.button(ol_menu_label(lk_icon, lk_text, "", OlTint::Normal)).clicked() {
        *toggle_lock = Some((row.item, row.locked)); ui.close_menu();
    }
    let (ey_icon, ey_text) = if row.visible { (ph::EYE_SLASH, "Hide") } else { (ph::EYE, "Show") };
    if ui.button(ol_menu_label(ey_icon, ey_text, &keys.hide, OlTint::Normal)).clicked() {
        *toggle_vis = Some((row.item, row.visible)); ui.close_menu();
    }
    if ui.button(ol_menu_label(ph::CROSSHAIR, "Frame Selected", &keys.focus, OlTint::Normal)).clicked() {
        *act = Some(OlAct::Frame); ui.close_menu();
    }
    if let OlItem::Node(n) = row.item {
        let nid = NodeId(n);
        ui.separator();
        ui.label(egui::RichText::new("HIERARCHY").size(9.5).color(ui_theme::TEXT_SECTION));
        if ui.button(ol_menu_label(ph::TREE_STRUCTURE, "Select Hierarchy", "", OlTint::Normal)).clicked() {
            *act = Some(OlAct::SelectHierarchy(nid)); ui.close_menu();
        }
        if row.has_children {
            if ui.button(ol_menu_label(ph::LIST, "Select Children", "", OlTint::Normal)).clicked() {
                *act = Some(OlAct::SelectChildren(nid)); ui.close_menu();
            }
        }
        if ui.button(ol_menu_label(ph::ARROW_UP, "Select Parent", "", OlTint::Normal)).clicked() {
            *act = Some(OlAct::SelectParent(nid)); ui.close_menu();
        }
        if row.has_children {
            if row.expanded {
                if ui.button(ol_menu_label(ph::CARET_RIGHT, "Collapse Branch", "", OlTint::Normal)).clicked() {
                    *act = Some(OlAct::CollapseBranch(n)); ui.close_menu();
                }
            } else if ui.button(ol_menu_label(ph::CARET_DOWN, "Expand Branch", "", OlTint::Normal)).clicked() {
                *act = Some(OlAct::ExpandBranch(n)); ui.close_menu();
            }
        }
    }
    if matches!(row.item, OlItem::Prim(_)) {
        ui.separator();
        ui.label(egui::RichText::new("HIERARCHY").size(9.5).color(ui_theme::TEXT_SECTION));
        if ui.button(ol_menu_label(ph::FOLDER_PLUS, "Parent (Group)", "", OlTint::Normal)).clicked() {
            *act = Some(OlAct::Parent); ui.close_menu();
        }
        if ui.button(ol_menu_label(ph::FOLDER_MINUS, "Unparent to New Body", "", OlTint::Normal)).clicked() {
            *act = Some(OlAct::Unparent); ui.close_menu();
        }
    }
    if matches!(row.menu, OlMenu::SdfBody { is_root_body: false }) {
        if ui.button(ol_menu_label(ph::ARROW_BEND_UP_LEFT, "Unparent Body to Root", "", OlTint::Normal)).clicked() {
            *act = Some(OlAct::Unparent); ui.close_menu();
        }
    }
    // Poly / imported mesh: un-nest to root when not already top-level (parent ≠ Root).
    if matches!(row.menu, OlMenu::Poly | OlMenu::Convertible) {
        if ui.button(ol_menu_label(ph::ARROW_BEND_UP_LEFT, "Unparent to Root", "", OlTint::Normal)).clicked() {
            *act = Some(OlAct::Unparent); ui.close_menu();
        }
    }
    ui.separator();
    if ui.button(ol_menu_label(ph::TRASH, "Delete", &keys.del, OlTint::Danger)).clicked() {
        *act = Some(OlAct::Delete); ui.close_menu();
    }
}

/// The eye's own right-click menu — visibility options the icon alone cannot express.
///
/// Only the three the backend can honour. The artifact's "Hide all" / "Invert visibility" /
/// "Show hierarchy" / "Hide hierarchy" have no backend and are NOT stubbed here: a menu item
/// that does nothing is worse than an absent one.
pub(crate) fn ol_eye_menu(ui: &mut egui::Ui, row: &OlRow, keys: &OlKeys,
               act: &mut Option<OlAct>,
               toggle_vis: &mut Option<(OlItem, bool)>) {
    ui.set_min_width(168.0);
    // "Isolate" is a TOGGLE — picking it while this row is already isolated restores the
    // group, exactly like a second alt+click. (The artifact's stateful "Exit isolate" label
    // would need a per-row solo probe every frame — an O(rows x siblings) scan the Frame
    // Budget Rule does not buy for a label.)
    if ui.button(ol_menu_label(ph::EYE, "Isolate", "Alt+click", OlTint::Normal)).clicked() {
        *act = Some(OlAct::Solo(row.item)); ui.close_menu();
    }
    ui.separator();
    let (ey_icon, ey_text) = if row.visible { (ph::EYE_SLASH, "Hide") } else { (ph::EYE, "Show") };
    if ui.button(ol_menu_label(ey_icon, ey_text, &keys.hide, OlTint::Normal)).clicked() {
        *toggle_vis = Some((row.item, row.visible)); ui.close_menu();
    }
    if ui.button(ol_menu_label(ph::EYE, "Show all", "", OlTint::Normal)).clicked() {
        *act = Some(OlAct::ShowAll(row.item)); ui.close_menu();
    }
}

/// Short name of a boolean op for the menu's trailing hint. Deliberately the BASE op only —
/// the hint answers "what is this prim doing to the body", not "with which k".
pub(crate) fn ol_bool_op_name(op: &core_math::primitives::BooleanOp) -> &'static str {
    use core_math::primitives::BooleanOp::*;
    match op {
        Union { .. } => "Union", ChamferUnion { .. } => "Union",
        Subtract { .. } => "Subtract", ChamferSubtract { .. } => "Subtract",
        Intersect => "Intersect", SmoothIntersect { .. } => "Intersect",
        Seam { .. } => "Seam", Extrude { .. } => "Extrude",
    }
}