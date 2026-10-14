// AppCommand + CommandPalette — extracted from types.rs
use glam::Vec3;
use core_math::primitives::BooleanOp;
use crate::*;

// ==================================================================
// AppCommand Queue — decouples UI intent from state mutation
// ==================================================================
#[derive(Clone)]
pub enum AppCommand {
    DeleteSelected,
    DuplicateSelected,
    SelectSingle(u32),            // prim_uid
    DeselectAll,
    SelectAll,
    SetBooleanOp(u32, BooleanOp), // prim_uid
    SetVisible(u32, bool),        // prim_uid
    SetLocked(u32, bool),         // prim_uid
    CreateInstance,
    /// Instance selected prim into every other top-level SDF body (linked).
    InstanceIntoAllBodies,
    MakeInstanceUnique(u32),      // prim_uid
    /// Break link on all selected instance prims.
    MakeSelectedInstancesUnique,
    Toast(String),
    Undo,
    Redo,
    // Phase 2: Command palette actions
    PolyExtrude, PolyInset, PolyLoopCut, PolyMerge, PolyDeleteFaces, PolySubdivide, PolyDice,
    PolyKnife, PolyBevelEdge, PolyUnbevel, PolyFaceHinge, PolyEdgeLoop, PolyEdgeRing, PolyBridge, PolySolidify, PolyCircle,
    PolyPoke, PolyConnect, PolyToSphere,
    NurbsKnotU, NurbsKnotV, NurbsWeightUp, NurbsWeightDown, NurbsGrabCv,
    ToggleWindow(&'static str),
    SetWorkMode(WorkMode),
    OpenCommandPalette,
    OpenLayoutSettings,
    // Phase A: Command migration — route direct mutations through commands
    SetCreateOp(CreateOp),
    // Phase B: Transform commands — logged immediately, not deferred
    SetPosition(u32, Vec3),       // prim_uid
    SetRotation(u32, Vec3),       // prim_uid
    SetScale(u32, Vec3),          // prim_uid
    // Compositor
    Screenshot,
}

// ==================================================================
// Command Palette (Phase 2)
// ==================================================================
pub struct CommandPaletteState {
    pub open: bool,
    pub search: String,
    pub selected_idx: usize,
}

impl Default for CommandPaletteState {
    fn default() -> Self { Self { open: false, search: String::new(), selected_idx: 0 } }
}

pub struct CommandDef {
    pub id: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    pub context: Option<&'static str>,
    pub shortcut_act: Option<&'static str>,
}

pub static COMMAND_DEFS: &[CommandDef] = &[
    // File
    CommandDef { id: "cmd_new",     name: "New Scene",          category: "File",  context: None, shortcut_act: Some(ACT_NEW_SCENE) },
    CommandDef { id: "cmd_open",    name: "Open Scene",         category: "File",  context: None, shortcut_act: Some(ACT_OPEN_SCENE) },
    CommandDef { id: "cmd_save",    name: "Save Scene",         category: "File",  context: None, shortcut_act: Some(ACT_SAVE_SCENE) },
    CommandDef { id: "cmd_saveas",  name: "Save As...",         category: "File",  context: None, shortcut_act: Some(ACT_SAVE_AS) },
    CommandDef { id: "cmd_export",  name: "Export...",          category: "File",  context: None, shortcut_act: Some(ACT_EXPORT) },
    // Edit
    CommandDef { id: "cmd_undo",    name: "Undo",              category: "Edit",  context: None, shortcut_act: Some(ACT_UNDO) },
    CommandDef { id: "cmd_redo",    name: "Redo",              category: "Edit",  context: None, shortcut_act: Some(ACT_REDO) },
    CommandDef { id: "cmd_delete",  name: "Delete Selected",   category: "Edit",  context: None, shortcut_act: Some(ACT_DELETE) },
    CommandDef { id: "cmd_dup",     name: "Duplicate",         category: "Edit",  context: None, shortcut_act: Some(ACT_DUPLICATE) },
    CommandDef { id: "cmd_selall",  name: "Select All",        category: "Edit",  context: None, shortcut_act: Some(ACT_SELECT_ALL) },
    // Transform
    CommandDef { id: "cmd_grab",    name: "Grab (Move)",       category: "Transform", context: None, shortcut_act: Some(ACT_GRAB) },
    CommandDef { id: "cmd_rotate",  name: "Rotate",            category: "Transform", context: None, shortcut_act: Some(ACT_ROTATE) },
    CommandDef { id: "cmd_scale",   name: "Scale",             category: "Transform", context: None, shortcut_act: Some(ACT_SCALE) },
    CommandDef { id: "cmd_focus",   name: "Focus Selected",    category: "Transform", context: None, shortcut_act: Some(ACT_FOCUS) },
    CommandDef { id: "cmd_space",   name: "Toggle Local/Global", category: "Transform", context: None, shortcut_act: Some(ACT_TOGGLE_SPACE) },
    // View
    CommandDef { id: "cmd_vfront",  name: "View Front",        category: "View",  context: None, shortcut_act: Some(ACT_VIEW_FRONT) },
    CommandDef { id: "cmd_vright",  name: "View Right",        category: "View",  context: None, shortcut_act: Some(ACT_VIEW_RIGHT) },
    CommandDef { id: "cmd_vtop",    name: "View Top",          category: "View",  context: None, shortcut_act: Some(ACT_VIEW_TOP) },
    CommandDef { id: "cmd_ortho",   name: "Toggle Ortho",      category: "View",  context: None, shortcut_act: Some(ACT_ORTHO_TOGGLE) },
    CommandDef { id: "cmd_wpplace", name: "Place Work Plane",  category: "View",  context: None, shortcut_act: Some(ACT_WORKPLANE_PLACE) },
    // Tools
    CommandDef { id: "cmd_drop",    name: "Drop Tool / Cancel",category: "Tools", context: None, shortcut_act: Some(ACT_DROP_TOOL) },
    CommandDef { id: "cmd_addmenu", name: "Add Object Menu",   category: "Tools", context: None, shortcut_act: Some(ACT_ADD_MENU) },
    CommandDef { id: "cmd_bevel",   name: "Toggle Bevel Mode", category: "Tools", context: Some("Object mode"), shortcut_act: Some(ACT_BEVEL_TOGGLE) },
    CommandDef { id: "cmd_extrude_mode", name: "Toggle Extrude Mode", category: "Tools", context: Some("Object mode"), shortcut_act: Some(ACT_EXTRUDE_TOGGLE) },
    CommandDef { id: "cmd_pivot",   name: "Toggle Pivot Edit", category: "Tools", context: None, shortcut_act: Some(ACT_PIVOT_EDIT) },
    CommandDef { id: "cmd_mirror",  name: "Smart Mirror",      category: "Tools", context: None, shortcut_act: Some(ACT_SMART_MIRROR) },
    CommandDef { id: "cmd_boolsub", name: "Boolean Subtract",  category: "Tools", context: None, shortcut_act: Some(ACT_BOOL_SUB) },
    CommandDef { id: "cmd_booladd", name: "Boolean Add",       category: "Tools", context: None, shortcut_act: Some(ACT_BOOL_ADD) },
    CommandDef { id: "cmd_boolint", name: "Boolean Intersect", category: "Tools", context: None, shortcut_act: Some(ACT_BOOL_INT) },
    // ACT_CMD_BAR is Cancel/Deselect (default Space) — not the agent bar.
    CommandDef { id: "cmd_cmdbar",  name: "Cancel / Deselect", category: "Tools", context: None, shortcut_act: Some(ACT_CMD_BAR) },
    // Same action as Group/Object (ACT_LAYER); shortcut is create_layer bind (Ctrl+Alt+L default).
    CommandDef { id: "cmd_layer",   name: "Group / Layer",     category: "Tools", context: Some("SDF selection"), shortcut_act: Some(ACT_LAYER) },
    CommandDef { id: "cmd_inst",    name: "Create Instance",   category: "Tools", context: None, shortcut_act: Some(ACT_INSTANCE) },
    CommandDef { id: "cmd_inst_all_bodies", name: "Instance Into All Bodies", category: "Tools", context: Some("SDF"), shortcut_act: None },
    CommandDef { id: "cmd_inst_break", name: "Break Instance Link (Make Unique)", category: "Tools", context: Some("SDF"), shortcut_act: None },
    // Poly context
    CommandDef { id: "cmd_poly_extrude",   name: "Poly: Extrude Face",     category: "Poly",  context: Some("Poly Face mode"),   shortcut_act: Some(ACT_POLY_EXTRUDE) },
    CommandDef { id: "cmd_poly_inset",     name: "Poly: Inset Face",       category: "Poly",  context: Some("Poly Face mode"),   shortcut_act: Some(ACT_POLY_INSET) },
    // shortcut_act is None: Circle's Alt+C lives in InputSystem (input_system.rs
    // ACTION_POLY_CIRCLE_REGION), not this legacy keybind registry.
    CommandDef { id: "cmd_poly_circle",    name: "Poly: Circle (Round Region)", category: "Poly", context: Some("Poly Face mode"), shortcut_act: None },
    CommandDef { id: "cmd_poly_loop_cut",  name: "Poly: Loop Cut",         category: "Poly",  context: Some("Poly Edge mode"),   shortcut_act: Some(ACT_POLY_LOOP_CUT) },
    CommandDef { id: "cmd_poly_merge",     name: "Poly: Merge Vertices",   category: "Poly",  context: Some("Poly Vertex mode"), shortcut_act: Some(ACT_POLY_MERGE) },
    CommandDef { id: "cmd_poly_knife",     name: "Poly: Knife",            category: "Poly",  context: Some("Polygonal mode"),   shortcut_act: Some(ACT_POLY_KNIFE) },
    CommandDef { id: "cmd_poly_bevel",     name: "Poly: Bevel Edge",       category: "Poly",  context: Some("Polygonal mode"),   shortcut_act: Some(ACT_POLY_BEVEL) },
    CommandDef { id: "cmd_poly_unbevel",   name: "Poly: Unbevel / Resize Bevel", category: "Poly", context: Some("Poly edit mode"), shortcut_act: Some(ACT_POLY_UNBEVEL) },
    CommandDef { id: "cmd_poly_face_hinge", name: "Poly: Face Hinge Rotate", category: "Poly", context: Some("Poly Face mode"), shortcut_act: Some(ACT_POLY_FACE_HINGE) },
    CommandDef { id: "cmd_poly_delete",    name: "Poly: Delete Faces",     category: "Poly",  context: Some("Poly Face mode"),   shortcut_act: Some(ACT_POLY_DELETE_FACE) },
    CommandDef { id: "cmd_poly_subdivide", name: "Poly: Subdivide (CC)",   category: "Poly",  context: Some("Polygonal mode"),   shortcut_act: Some(ACT_POLY_SUBDIVIDE) },
    CommandDef { id: "cmd_poly_edge_loop", name: "Poly: Select Edge Loop", category: "Poly",  context: Some("Poly Edge mode"),   shortcut_act: Some(ACT_POLY_EDGE_LOOP) },
    CommandDef { id: "cmd_poly_edge_ring", name: "Poly: Select Edge Ring", category: "Poly",  context: Some("Poly Edge mode"),   shortcut_act: Some(ACT_POLY_EDGE_RING) },
    // NURBS context
    CommandDef { id: "cmd_nurbs_knot_u",     name: "NURBS: Insert Knot U",  category: "NURBS", context: Some("NURBS mode"),   shortcut_act: Some(ACT_NURBS_KNOT_U) },
    CommandDef { id: "cmd_nurbs_knot_v",     name: "NURBS: Insert Knot V",  category: "NURBS", context: Some("NURBS mode"),   shortcut_act: Some(ACT_NURBS_KNOT_V) },
    CommandDef { id: "cmd_nurbs_weight_up",  name: "NURBS: Weight Up",      category: "NURBS", context: Some("NURBS CV mode"),shortcut_act: Some(ACT_NURBS_WEIGHT_UP) },
    CommandDef { id: "cmd_nurbs_weight_down",name: "NURBS: Weight Down",    category: "NURBS", context: Some("NURBS CV mode"),shortcut_act: Some(ACT_NURBS_WEIGHT_DOWN) },
    CommandDef { id: "cmd_nurbs_grab_cv",    name: "NURBS: Grab CVs",       category: "NURBS", context: Some("NURBS CV mode"),shortcut_act: Some(ACT_NURBS_GRAB_CV) },
    // UI toggles
    CommandDef { id: "cmd_show_tweak",    name: "Show Developer Tweaks",   category: "UI", context: None, shortcut_act: None },
    CommandDef { id: "cmd_show_shortcuts",name: "Show Shortcut Editor",    category: "UI", context: None, shortcut_act: Some(ACT_SHORTCUT_EDITOR) },
    CommandDef { id: "cmd_show_export",   name: "Show Export Window",      category: "UI", context: None, shortcut_act: Some(ACT_EXPORT) },
    CommandDef { id: "cmd_show_grid",     name: "Show Grid Settings",      category: "UI", context: None, shortcut_act: None },
    CommandDef { id: "cmd_show_layout",   name: "Show Layout Settings",    category: "UI", context: None, shortcut_act: Some(ACT_LAYOUT_SETTINGS) },
    CommandDef { id: "cmd_dump_ui_layout", name: "Dump UI Layout (debug)", category: "UI", context: None, shortcut_act: None },
    CommandDef { id: "cmd_toggle_toolpanel", name: "Toggle Tool Panel",    category: "UI", context: None, shortcut_act: None },
    CommandDef { id: "cmd_toggle_props",  name: "Toggle Properties Panel", category: "UI", context: None, shortcut_act: None },
    CommandDef { id: "cmd_toggle_header", name: "Toggle Viewport Header",  category: "UI", context: None, shortcut_act: None },
    // Mode switching
    CommandDef { id: "cmd_mode_obj",    name: "Switch to Object Mode",  category: "Mode", context: None, shortcut_act: None },
    CommandDef { id: "cmd_mode_sculpt", name: "Switch to Sculpt Mode",  category: "Mode", context: None, shortcut_act: None },
    CommandDef { id: "cmd_mode_face",   name: "Switch to Face Mode",    category: "Mode", context: None, shortcut_act: None },
    CommandDef { id: "cmd_palette",     name: "Command Palette",        category: "UI",   context: None, shortcut_act: Some(ACT_COMMAND_PALETTE) },
    // Phase A/B/D commands
    CommandDef { id: "cmd_set_op_union",    name: "Set Op: Union",         category: "Boolean", context: None, shortcut_act: Some(ACT_BOOL_ADD) },
    CommandDef { id: "cmd_set_op_subtract", name: "Set Op: Subtract",     category: "Boolean", context: None, shortcut_act: Some(ACT_BOOL_SUB) },
    CommandDef { id: "cmd_set_op_intersect",name: "Set Op: Intersect",    category: "Boolean", context: None, shortcut_act: Some(ACT_BOOL_INT) },
    CommandDef { id: "cmd_macro_toggle",    name: "Toggle Macro Recording",category: "Macro",  context: None, shortcut_act: None },
    CommandDef { id: "cmd_macro_play",      name: "Play Last Macro",       category: "Macro",  context: None, shortcut_act: None },
    CommandDef { id: "cmd_macro_save",      name: "Save Macro...",         category: "Macro",  context: None, shortcut_act: None },
    CommandDef { id: "cmd_macro_load",      name: "Load Macro...",         category: "Macro",  context: None, shortcut_act: None },
    CommandDef { id: "cmd_surface_sculpt", name: "Surface Sculpt (3D-Coat)", category: "Sculpt", context: Some("Mesh or voxel selected"), shortcut_act: None },
];
