//! Outliner types, layout constants, and pure display helpers.
use eframe::egui;
use egui_phosphor::regular as ph;
use crate::*;
use super::super::*; // OlCol, etc. from ui2

/// One flattened outliner row (scene root, node or prim).
#[derive(Clone)]
pub(crate) struct OlRow {
    pub(crate) item: OlItem,
    pub(crate) depth: usize,
    pub(crate) prefix: Vec<bool>,   // per ancestor column: draw a continuing vertical line
    pub(crate) is_last: bool,       // last child of its parent (elbow vs tee)
    pub(crate) is_root: bool,       // the Scene root row
    pub(crate) name: String,
    pub(crate) icon: &'static str,
    pub(crate) color: egui::Color32,
    pub(crate) visible: bool,
    pub(crate) locked: bool,
    pub(crate) has_children: bool,
    pub(crate) expanded: bool,
    /// Boolean-op badge: (colour, glyph). When set it REPLACES the type icon — a row is
    /// `chev + badge + name` or `chev + icon + name`, never all three.
    pub(crate) badge: Option<(egui::Color32, &'static str)>,
    pub(crate) mods: u32,
    pub(crate) tex: u32,
    /// This body carries a UV layer. A BOOL, not a count: `CustomData` holds exactly ONE
    /// corner-domain `uv` LayerStore — there is no plural "UV set" concept to count.
    pub(crate) uv: bool,
    pub(crate) selected: bool,
    /// This row's key into the shared rename state (`outliner.renaming`), or `None` when the
    /// row has no stored name (the Scene root). See `ol_rename_target`.
    pub(crate) rename: Option<SceneNodeId>,
    /// What this row's context menu offers.
    pub(crate) menu: OlMenu,
    /// This row IS the body the artist is editing (`scene.active_body_nid`).
    pub(crate) active_body: bool,
    /// Poly manifold status when Manifold column is active; `None` = N/A.
    pub(crate) manifold: Option<bool>,
}

/// The contextual HEAD of a row's context menu — resolved in `ol_build` from the payload.
#[derive(Clone)]
pub(crate) enum OlMenu {
    /// The Scene root — no name, no owner, no delete path. It gets NO menu at all.
    Root,
    Prim {
        op: core_math::primitives::BooleanOp,
        shape: String,
        is_base: bool,
        /// `Some(master_uid)` when this prim is a linked instance.
        instance_master: Option<u32>,
    },
    SdfBody { is_root_body: bool },
    /// Editable polygonal body — subdivide / convert-to-SDF / export / Surface Sculpt.
    Poly,
    /// VoxelBody — 3D-Coat Surface switch (mesh displace) plus the common tail.
    Voxel,
    /// ImportedMesh / NurbsBody — convert_*_to_poly (+ export for nurbs).
    Convertible,
    /// Kitbash Group folder — save group to library.
    Group,
    /// Light — type can be changed (Directional / Point / Spot).
    Light {
        id: u32,
        light_type: core_math::scene_graph::LightType,
    },
    /// Graph material node (Materials section / tree). `mid` = MaterialId.0.
    Material { mid: u32 },
    /// Map-channel child under a material (not independently selectable/renameable).
    MatMap {
        parent: NodeId,
        kind: core_math::pbr_material::MapKind,
    },
    /// Camera / voxel / empty / SdfObject — the common tail only.
    Generic,
}

/// A context-menu action, collected during the row draw and applied AFTER it.
///
/// The menu must not mutate the scene mid-list: `rows` was flattened from the pre-mutation
/// graph, so every row below the click would paint from stale data, and a structural action
/// (group / dissolve / delete) would leave the rest of the loop chasing dangling ids.
/// Duplicate / Delete / Group carry no target — they are selection-driven. Right-click does
/// NOT replace an existing selection (owner 2026-08-06); only empty selection adopts the row.
pub(crate) enum OlAct {
    /// (target, current name) — opens the inline field; the same state the double-click sets.
    StartRename(SceneNodeId, String),
    Duplicate,
    Delete,
    Group,
    Dissolve(NodeId),
    ConvertToPoly(NodeId),
    SaveAsPart(NodeId),
    EnterScope(NodeId),
    SetBool(u32, core_math::primitives::BooleanOp),
    SetShape(u32, core_math::primitives::SdfShape),
    AddMod(u32, core_math::primitives::ModifierType),
    Solo(OlItem),
    ShowAll(OlItem),
    /// Frame camera + scroll outliner to selection.
    Frame,
    ExpandBranch(u64),
    CollapseBranch(u64),
    /// Make this SDF body the active edit target (amber bar).
    SetActiveBody(NodeId),
    SelectChildren(NodeId),
    SelectHierarchy(NodeId),
    SelectParent(NodeId),
    Unparent,
    Parent,
    SetLightType(u32, core_math::scene_graph::LightType),
    CreateMaterial,
    DuplicateMaterial(NodeId),
    DeleteMaterial(NodeId),
    /// Assign this graph material (`MaterialId.0`) to the current selection (prims / body / faces).
    AssignMaterialToSelection(u32),
    /// Select all poly faces (and SDF prims) that use this material.
    SelectByMaterial(u32),
    ClearMap(NodeId, core_math::pbr_material::MapKind),
    /// Focus properties on a material map channel (select parent + set focus_map_channel).
    /// `time` is egui `input.time` so the highlight timer matches the properties panel.
    FocusMapChannel(NodeId, core_math::pbr_material::MapKind, f64),
    // ── Instance (prim) ──
    CreateInstance(u32),
    MakeUnique(u32),
    SelectMaster(u32),
    InstanceIntoAll(u32),
    // ── Poly / mesh ──
    Subdivide(NodeId),
    ConvertToSdf(NodeId),
    LinkAsSdf(NodeId),
    ExportObj(NodeId),
    BakeMeshMaps(NodeId),
    /// 3D-Coat Surface: sculpt the poly mesh, or switch a voxel object to Surface mode.
    SurfaceSculpt(NodeId),
}
/// What a row click does to the selection. Shift beats Ctrl — a range is the more specific
/// intent, and Ctrl+Shift means "extend the range" in every DCC, not "toggle".
#[derive(Clone, Copy)]
pub(crate) enum SelMode { Single, Toggle, Range }

#[derive(Clone, Copy)]
pub(crate) enum OlItem { Node(u64), Prim(u32) }
impl OlItem {
    /// The unified-selection id for this row. ui2 rows carry `OlItem`; every selection write
    /// takes `SceneItemId` — this is the ONE conversion (never a flat prim index).
    pub(crate) fn sel_id(self) -> SceneItemId {
        match self {
            OlItem::Node(n) => SceneItemId::Node(NodeId(n)),
            OlItem::Prim(u) => SceneItemId::Prim(u),
        }
    }
}

// ── Outliner grid: `30 | max(168, 1fr) | 32*N | 44` ──────────────────────────
pub(crate) const OL_ROW_H: f32 = ui_theme::ROW_HEIGHT;   // 26
/// Quick-bar strip docked under the tree (Duplicate / Res− / Res+). Reserved BEFORE
/// the scroll area so the tree can never scroll under it.
pub(crate) const OL_QUICKBAR_H: f32 = 26.0;
pub(crate) const OL_EYE_W: f32 = 30.0;
pub(crate) const OL_LOCK_W: f32 = 44.0;                  // the trailing "acts" COLUMN…
pub(crate) const OL_LOCK_BTN: egui::Vec2 = egui::vec2(22.0, 20.0); // …and the chip inside it
pub(crate) const OL_NAME_MIN: f32 = 168.0;
pub(crate) const OL_INFO_COL_W: f32 = 32.0;
pub(crate) const OL_INDENT: f32 = 17.0;
/// Indent origin (8) and connector-guide origin (13 = row_h/2 = the chevron's half-width)
/// share the 17px step but are NOT the same number. Do not fold them together.
pub(crate) const OL_INDENT_BASE: f32 = 8.0;
pub(crate) const OL_GUIDE_X0: f32 = 13.0;
pub(crate) const OL_ELBOW_W: f32 = 9.0;
pub(crate) const OL_CHEV_W: f32 = 12.0;
pub(crate) const OL_GAP: f32 = 7.0;
/// Icon/badge slot. The artifact's `.tico`/`.boolbadge` are unpadded grid boxes that size to
/// their glyph (15 / 13px); one 16px slot holds either and keeps the name aligned across rows.
pub(crate) const OL_ICON_W: f32 = 16.0;

/// How a context-menu item is tinted.
#[derive(Clone, Copy)]
pub(crate) enum OlTint {
    /// Dim icon, secondary label — the default.
    Normal,
    /// The icon carries a DATA hue (the artifact tints the Boolean icon by its op); the label
    /// stays normal so the row still reads as an ordinary item.
    Icon(egui::Color32),
    /// Destructive: icon AND label in DESTRUCTIVE — never DANGER, which is reserved for the
    /// hidden eye and the subtract badge.
    Danger,
}

/// Shortcut hints for the menu, resolved from the LIVE keybinding registry once per frame.
/// Never hardcode these — user-rebindable (incl. ACT_RENAME / ACT_FOCUS).
pub(crate) struct OlKeys {
    pub(crate) dup: String,
    pub(crate) del: String,
    pub(crate) hide: String,
    pub(crate) rename: String,
    pub(crate) focus: String,
}

/// The presets "Change Shape" offers, as indices into `shape_preset()` — Sphere, Box, Cylinder,
/// Torus, Capsule, Cone. Same set and order as the viewport's Change Shape (menus.rs:836); both
/// derive the shapes themselves from the ONE source (`shape_preset` / `curve_preset`), and the
/// labels/icons from the shape data, so no shape literal is spelled out twice.
pub(crate) const OL_SHAPE_PRESETS: [usize; 6] = [0, 1, 2, 3, 5, 4];

/// How many info columns fit before Name would drop under its 168 floor (the artifact's fitCols).
pub(crate) fn ol_fit_cols(avail: f32, active: usize) -> usize {
    let room = avail - OL_EYE_W - OL_LOCK_W - OL_NAME_MIN;
    ((room / OL_INFO_COL_W).floor().max(0.0) as usize).min(active)
}

/// One info cell. Non-applicable and zero both render `Empty` — the artifact never prints "0".
pub(crate) fn ol_cell(c: OlCol, row: &OlRow) -> OlCell {
    if row.is_root { return OlCell::Empty; }
    match c {
        OlCol::Mod => if row.mods > 0 { OlCell::Num(row.mods) } else { OlCell::Empty },
        OlCol::Tex => if row.tex > 0 { OlCell::Num(row.tex) } else { OlCell::Empty },
        // A dot, not a number: there is exactly one UV layer per body or none — nothing to count.
        OlCol::Uv => if row.uv { OlCell::Dot(ui_theme::MESH) } else { OlCell::Empty },
        // Anim: no tracks / timeline in AZRA yet — always empty (column tip explains).
        OlCol::Anim => OlCell::Empty,
        // Manifold: poly bodies only.
        // OK = quiet green dot; broken = DANGER warning glyph (column tip: "warning if broken").
        // A green dot for broken was the old look and made the column look "always fine".
        OlCol::Manifold => match row.manifold {
            Some(true) => OlCell::Dot(ui_theme::SDF),
            Some(false) => OlCell::Warn(ui_theme::DANGER),
            None => OlCell::Empty,
        },
    }
}

/// Tree connectors. The artifact draws these 1px *dotted*; egui has no dotted stroke, so
/// this is solid at the artifact's exact hue (see the report — dashing was not invented).
pub(crate) const OL_TREE_LINE: egui::Color32 = egui::Color32::from_rgb(0x3a, 0x3a, 0x3e);
/// `.n-root` name colour — brighter than TEXT, root row only.
pub(crate) const OL_TEXT_ROOT: egui::Color32 = egui::Color32::from_rgb(0xf0, 0xf1, 0xf2);

/// Typed icon + colour for a scene node (artifact "AZRA — App Mockup Target").
/// The icon hue is depth-INDEPENDENT — an SDF body and its nested prims are both `--sdf`.
/// Depth shows in the NAME instead (see `ol_name_style`), never in the icon.
pub(crate) fn ol_node_visual(payload: &NodePayload) -> (&'static str, egui::Color32) {
    match payload {
        NodePayload::SdfBody { .. } | NodePayload::SdfObject { .. } => (ph::CIRCLES_THREE, ui_theme::SDF),
        NodePayload::Group { name } if name.starts_with("NURBS.") => (ph::CUBE, egui::Color32::from_rgb(200, 140, 255)),
        NodePayload::Group { .. } => (ph::FOLDER, ui_theme::FOLDER_GOLD),
        NodePayload::Camera { .. } => (ph::VIDEO_CAMERA, ui_theme::CAM),
        NodePayload::Light(data) => (
            if matches!(data.light_type, core_math::scene_graph::LightType::Directional) { ph::SUN } else { ph::LIGHTBULB },
            ui_theme::LIGHT),
        NodePayload::ImportedMesh { .. } | NodePayload::PolygonalBody { .. } => (ph::CUBE, ui_theme::MESH),
        NodePayload::NurbsBody { .. } => (ph::SPHERE, egui::Color32::from_rgb(200, 140, 255)),
        NodePayload::NurbsSolid { .. } => (ph::CUBE, egui::Color32::from_rgb(200, 140, 255)),
        NodePayload::VoxelBody { .. } => (ph::SPHERE, ui_theme::SCULPT_ORANGE),
        NodePayload::Empty { .. } => (ph::BOUNDING_BOX, ui_theme::TEXT_DIM),
        NodePayload::Material { .. } => (ph::PAINT_BRUSH, egui::Color32::from_rgb(200, 140, 220)),
        _ => (ph::CUBE, ui_theme::TEXT_DIM),
    }
}

// OlCol lives in types.rs (Ui2State).

/// What one info cell shows. `Empty` still draws the cell's separator.
#[derive(Clone, Copy)]
pub(crate) enum OlCell {
    Empty,
    Num(u32),
    Dot(egui::Color32),
    /// Manifold-broken (and similar): draw the column's warning glyph in `Color32`.
    Warn(egui::Color32),
}

/// `.n-root` / `.n-body` / `.n-child` — (size, semibold, colour). Depth lives here, not in the icon.
pub(crate) fn ol_name_style(row: &OlRow) -> (f32, bool, egui::Color32) {
    if row.is_root { return (14.0, true, OL_TEXT_ROOT); }
    match row.item {
        // `.n-child n-sdf` — a prim inside an SDF body gets a GREEN name; the body row itself
        // is plain `.n-body` and stays TEXT.
        OlItem::Prim(_) => (12.5, false, ui_theme::SDF),
        OlItem::Node(_) if row.depth == 0 => (13.0, true, ui_theme::TEXT),
        OlItem::Node(_) => (12.5, false, ui_theme::TEXT_SECONDARY),
    }
}

/// Phosphor glyph for an SDF primitive's base shape.
pub(crate) fn prim_shape_icon(shape: &core_math::primitives::SdfShape) -> &'static str {
    use core_math::primitives::SdfShape::*;
    match shape {
        Sphere { .. } => ph::CIRCLE,
        Box { .. } => ph::CUBE,
        Cylinder { .. } => ph::CYLINDER,
        Cone { .. } => ph::TRIANGLE,
        Capsule { .. } => ph::PILL,
        Torus { .. } => ph::CIRCLE,
        _ => ph::CUBE,
    }
}

/// Boolean-op → (badge colour, Phosphor glyph). The artifact defines exactly three badges;
/// smooth/chamfer variants fold into their base op rather than inventing a fourth.
/// `None` = no badge, so the row falls back to its shape icon (the artifact's `it.bool ? … : icon`).
/// Seam/Extrude deliberately return None: they displace the surface rather than add/cut/keep
/// volume, so stamping a "Union (add)" badge on them would misdescribe the geometry.
pub(crate) fn op_visual(op: &core_math::primitives::BooleanOp) -> Option<(egui::Color32, &'static str)> {
    use core_math::primitives::BooleanOp::*;
    match op {
        Union { .. } | ChamferUnion { .. } => Some((ui_theme::SDF, ph::UNITE)),
        Subtract { .. } | ChamferSubtract { .. } => Some((ui_theme::DANGER, ph::SUBTRACT)),
        Intersect | SmoothIntersect { .. } => Some((ui_theme::WARNING, ph::INTERSECT)),
        Seam { .. } | Extrude { .. } => None,
    }
}
