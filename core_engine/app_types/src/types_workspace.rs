// Workspace System — ToolButton + Panel + Workspace — extracted from types.rs
use serde::{Serialize, Deserialize};

/// Deserialize a panel's button list, DROPPING ids this build does not know instead of
/// failing the whole document.
///
/// Why this exists: `WorkspaceManager::load` does
/// `serde_json::from_str(&json).unwrap_or_else(|_| Self::defaults())`, so ONE unrecognised
/// button string discarded EVERY workspace in `~/.azra/workspaces.json` — renamed builtins,
/// custom workspaces, panel layout, the lot — with no error shown. The next `save()` then
/// wrote the defaults back, making the loss permanent. `defaults()` cannot reconstruct a
/// renamed builtin, so the data was simply gone.
///
/// That made retiring ANY `ToolButtonId` a silent data-corruption event (CLAUDE.md priority
/// ladder #1) on machines that had customised their toolbars — i.e. exactly the users who
/// would notice. Dropping an unknown button is also the semantically correct answer: the
/// button does not exist in this build, so it cannot be shown, but nothing else about the
/// user's layout is invalidated by that.
fn buttons_skip_unknown<'de, D>(d: D) -> Result<Vec<ToolButtonId>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Land in a self-describing value first so a bad element cannot poison its siblings —
    // a plain Vec<ToolButtonId> aborts the whole sequence on the first unknown variant.
    let raw = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|v| serde_json::from_value::<ToolButtonId>(v).ok())
        .collect())
}

// ==================================================================
// Workspace System — ToolButton + Panel + Workspace
// ==================================================================

/// Every possible toolbar button in the app.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ToolButtonId {
    // Mode switches
    ModeObject, ModeSculpt, ModeFace,
    // SDF shape creation (index 0-7 = footprint create presets)
    ShapeSphere, ShapeBox, ShapeCylinder, ShapeTorus,
    ShapeCone, ShapeCapsule, ShapeStair, ShapeSpiral,
    // Path / curve SDF tools (not footprint-create presets — dedicated entry)
    // NOTE: append-only for workspace JSON; never reorder/remove variants (skip-unknown loader).
    ShapePolyline, ShapeSpine,
    /// Surface boolean cut tool (draw thin box cutter on hit surface).
    ToolBoolCut,
    /// Removed feature (was French Curve cutter). Kept for workspace JSON append-only layout.
    #[doc(hidden)]
    ShapeFrenchCurve,
    // Boolean ops
    OpSubtract, OpUnion, OpIntersect, OpSlice,
    // Gizmo
    GizmoMove, GizmoRotate, GizmoScale,
    // Sculpt brushes
    BrushAdd, BrushCarve, BrushSmooth, BrushFlatten, BrushInflate,
    BrushPinch, BrushClay, BrushCrease, BrushTrim,
    // Convert
    ConvertToClay,
    // Poly sub-modes
    PolyModeObj, PolyModeVtx, PolyModeEdg, PolyModeFce,
    // Poly edit tools
    PolyExtrude, PolyBevel, PolyLoopCut, PolyKnife, PolyMerge, PolyInset,
    // Poly mesh creation
    PolyCreateBox, PolyCreatePlane, PolyCreateSphere,
    PolyCreateCylinder, PolyCreateTorus, PolyCreateCone,
    // NURBS sub-modes
    NurbsModeObj, NurbsModeCv, NurbsModeSrf,
    // NURBS edit tools
    NurbsKnotU, NurbsKnotV, NurbsWeight, NurbsLoft, NurbsRevolve,
    // NURBS surface creation
    NurbsCreatePlane, NurbsCreateCylinder, NurbsCreateSphere, NurbsCreateTorus,
    // Visual elements
    Separator,
    Label(u8), // 0=MODE, 1=SHAPE, 2=OP, 3=TOOL, 4=BRUSH, 5=EDIT, 6=CREATE
    // Generic command (index into COMMAND_DEFS)
    Command(u8),
}

/// Static metadata for rendering a toolbar button.
pub struct ToolButtonMeta {
    pub icon: &'static str,
    pub tooltip: &'static str,
    pub width: f32,
    pub height: f32,
}

/// Returns rendering metadata for a ToolButtonId.
pub fn tool_button_meta(id: ToolButtonId) -> ToolButtonMeta {
    use ToolButtonId::*;
    match id {
        // Mode (40x40)
        ModeObject  => ToolButtonMeta { icon: "\u{1F537}", tooltip: "Object Mode",  width: 40.0, height: 40.0 },
        ModeSculpt  => ToolButtonMeta { icon: "\u{1F58C}", tooltip: "Sculpt Mode",  width: 40.0, height: 40.0 },
        ModeFace    => ToolButtonMeta { icon: "\u{25A0}",  tooltip: "Face Push/Pull", width: 40.0, height: 40.0 },
        // Shapes (40x32)
        ShapeSphere   => ToolButtonMeta { icon: "\u{26AA}",  tooltip: "Sphere",   width: 40.0, height: 32.0 },
        ShapeBox      => ToolButtonMeta { icon: "\u{2B1C}",  tooltip: "Box",      width: 40.0, height: 32.0 },
        ShapeCylinder => ToolButtonMeta { icon: "\u{1F535}", tooltip: "Cylinder", width: 40.0, height: 32.0 },
        ShapeTorus    => ToolButtonMeta { icon: "\u{1F369}", tooltip: "Torus",    width: 40.0, height: 32.0 },
        ShapeCone     => ToolButtonMeta { icon: "\u{1F53A}", tooltip: "Cone",     width: 40.0, height: 32.0 },
        ShapeCapsule  => ToolButtonMeta { icon: "\u{1F48A}", tooltip: "Capsule",  width: 40.0, height: 32.0 },
        ShapeStair    => ToolButtonMeta { icon: "\u{1FA9C}", tooltip: "Stair",    width: 40.0, height: 32.0 },
        ShapeSpiral   => ToolButtonMeta { icon: "\u{1F300}", tooltip: "Spiral",   width: 40.0, height: 32.0 },
        ShapePolyline => ToolButtonMeta { icon: "\u{2312}",  tooltip: "Polyline (draw)", width: 40.0, height: 32.0 },
        ShapeSpine    => ToolButtonMeta { icon: "\u{223F}",  tooltip: "Spine / Pipe",    width: 40.0, height: 32.0 },
        ToolBoolCut   => ToolButtonMeta { icon: "\u{2702}",  tooltip: "Bool Cut (surface)", width: 40.0, height: 32.0 },
        ShapeFrenchCurve => ToolButtonMeta {
            icon: "\u{2B1C}",
            tooltip: "Box (legacy French Curve slot)",
            width: 40.0,
            height: 32.0,
        },
        // Boolean ops (40x24)
        OpSubtract  => ToolButtonMeta { icon: "X", tooltip: "Subtract", width: 40.0, height: 24.0 },
        OpUnion     => ToolButtonMeta { icon: "J", tooltip: "Union",     width: 40.0, height: 24.0 },
        OpIntersect => ToolButtonMeta { icon: "I", tooltip: "Intersect", width: 40.0, height: 24.0 },
        OpSlice     => ToolButtonMeta { icon: "S", tooltip: "Slice",     width: 40.0, height: 24.0 },
        // Gizmo (40x32)
        GizmoMove   => ToolButtonMeta { icon: "\u{2194}", tooltip: "Move (G)",   width: 40.0, height: 32.0 },
        GizmoRotate => ToolButtonMeta { icon: "\u{27F3}", tooltip: "Rotate (R)", width: 40.0, height: 32.0 },
        GizmoScale  => ToolButtonMeta { icon: "\u{2922}", tooltip: "Scale (S)",  width: 40.0, height: 32.0 },
        // Brushes (40x32)
        BrushAdd     => ToolButtonMeta { icon: "\u{2795}", tooltip: "Add Clay",  width: 40.0, height: 32.0 },
        BrushCarve   => ToolButtonMeta { icon: "\u{2796}", tooltip: "Carve",     width: 40.0, height: 32.0 },
        BrushSmooth  => ToolButtonMeta { icon: "\u{3030}", tooltip: "Smooth",    width: 40.0, height: 32.0 },
        BrushFlatten => ToolButtonMeta { icon: "\u{25AC}", tooltip: "Flatten",   width: 40.0, height: 32.0 },
        BrushInflate => ToolButtonMeta { icon: "\u{2B06}", tooltip: "Inflate",   width: 40.0, height: 32.0 },
        BrushPinch   => ToolButtonMeta { icon: "\u{25C7}", tooltip: "Pinch",     width: 40.0, height: 32.0 },
        BrushClay    => ToolButtonMeta { icon: "\u{25A4}", tooltip: "Clay",      width: 40.0, height: 32.0 },
        BrushCrease  => ToolButtonMeta { icon: "\u{2228}", tooltip: "Crease",    width: 40.0, height: 32.0 },
        BrushTrim    => ToolButtonMeta { icon: "\u{229F}", tooltip: "Trim",      width: 40.0, height: 32.0 },
        // Convert
        ConvertToClay => ToolButtonMeta { icon: "\u{1F525}", tooltip: "Convert to Clay (Sculpt)", width: 40.0, height: 32.0 },
        // Poly modes (40x28)
        PolyModeObj => ToolButtonMeta { icon: "Obj", tooltip: "Object Mode", width: 40.0, height: 28.0 },
        PolyModeVtx => ToolButtonMeta { icon: "Vtx", tooltip: "Vertex Mode", width: 40.0, height: 28.0 },
        PolyModeEdg => ToolButtonMeta { icon: "Edg", tooltip: "Edge Mode",   width: 40.0, height: 28.0 },
        PolyModeFce => ToolButtonMeta { icon: "Fce", tooltip: "Face Mode",   width: 40.0, height: 28.0 },
        // Poly tools (40x28)
        PolyExtrude => ToolButtonMeta { icon: "Ext", tooltip: "Extrude Face (E)", width: 40.0, height: 28.0 },
        PolyBevel   => ToolButtonMeta { icon: "Bvl", tooltip: "Bevel Edge (Ctrl+B)",  width: 40.0, height: 28.0 },
        PolyLoopCut => ToolButtonMeta { icon: "Cut", tooltip: "Loop Cut (Ctrl+R)", width: 40.0, height: 28.0 },
        PolyKnife   => ToolButtonMeta { icon: "Kni", tooltip: "Knife (K)",       width: 40.0, height: 28.0 },
        PolyMerge   => ToolButtonMeta { icon: "Mrg", tooltip: "Merge Vertices (M)", width: 40.0, height: 28.0 },
        PolyInset   => ToolButtonMeta { icon: "Ins", tooltip: "Inset Face (I)",  width: 40.0, height: 28.0 },
        // Poly create (40x28)
        PolyCreateBox      => ToolButtonMeta { icon: "\u{2B1C}",  tooltip: "Poly Box",      width: 40.0, height: 28.0 },
        PolyCreatePlane    => ToolButtonMeta { icon: "\u{25AC}",  tooltip: "Poly Plane",    width: 40.0, height: 28.0 },
        PolyCreateSphere   => ToolButtonMeta { icon: "\u{26AA}",  tooltip: "Poly Sphere",   width: 40.0, height: 28.0 },
        PolyCreateCylinder => ToolButtonMeta { icon: "\u{1F535}", tooltip: "Poly Cylinder", width: 40.0, height: 28.0 },
        PolyCreateTorus    => ToolButtonMeta { icon: "\u{1F369}", tooltip: "Poly Torus",    width: 40.0, height: 28.0 },
        PolyCreateCone     => ToolButtonMeta { icon: "\u{1F53A}", tooltip: "Poly Cone",     width: 40.0, height: 28.0 },
        // NURBS modes (40x28)
        NurbsModeObj => ToolButtonMeta { icon: "Obj", tooltip: "Object Mode", width: 40.0, height: 28.0 },
        NurbsModeCv  => ToolButtonMeta { icon: "CV",  tooltip: "Control Point Mode", width: 40.0, height: 28.0 },
        NurbsModeSrf => ToolButtonMeta { icon: "Srf", tooltip: "Surface Mode", width: 40.0, height: 28.0 },
        // NURBS tools (40x28)
        NurbsKnotU   => ToolButtonMeta { icon: "KnU", tooltip: "Insert Knot U", width: 40.0, height: 28.0 },
        NurbsKnotV   => ToolButtonMeta { icon: "KnV", tooltip: "Insert Knot V", width: 40.0, height: 28.0 },
        NurbsWeight  => ToolButtonMeta { icon: "Wgt", tooltip: "Adjust Weight", width: 40.0, height: 28.0 },
        NurbsLoft    => ToolButtonMeta { icon: "Lft", tooltip: "Loft",          width: 40.0, height: 28.0 },
        NurbsRevolve => ToolButtonMeta { icon: "Rev", tooltip: "Revolve",       width: 40.0, height: 28.0 },
        // NURBS create (40x28)
        NurbsCreatePlane    => ToolButtonMeta { icon: "\u{25AC}",  tooltip: "NURBS Plane",    width: 40.0, height: 28.0 },
        NurbsCreateCylinder => ToolButtonMeta { icon: "\u{1F535}", tooltip: "NURBS Cylinder", width: 40.0, height: 28.0 },
        NurbsCreateSphere   => ToolButtonMeta { icon: "\u{26AA}",  tooltip: "NURBS Sphere",   width: 40.0, height: 28.0 },
        NurbsCreateTorus    => ToolButtonMeta { icon: "\u{1F369}", tooltip: "NURBS Torus",    width: 40.0, height: 28.0 },
        // Visual
        Separator   => ToolButtonMeta { icon: "",   tooltip: "",           width: 0.0,  height: 0.0 },
        Label(_)    => ToolButtonMeta { icon: "",   tooltip: "",           width: 0.0,  height: 0.0 },
        Command(_)  => ToolButtonMeta { icon: "Cmd", tooltip: "Command",  width: 40.0, height: 28.0 },
    }
}

/// Where a panel is docked.
#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub enum PanelDock {
    Left,
    Right,
    Float { x: f32, y: f32 },
}

/// A panel containing an ordered list of buttons.
#[derive(Clone, Serialize, Deserialize)]
pub struct WorkspacePanel {
    pub id: String,
    pub name: String,
    /// Buttons this panel shows. Unknown IDs are DROPPED on load, never fatal — see
    /// `buttons_skip_unknown`. Retiring a `ToolButtonId` must cost the user that button,
    /// not their entire workspace file.
    #[serde(deserialize_with = "buttons_skip_unknown")]
    pub buttons: Vec<ToolButtonId>,
    pub dock: PanelDock,
    pub width: f32,
    pub collapsed: bool,
    #[serde(default)]
    pub builtin_default: bool, // true = came with a built-in workspace, can't delete
}

/// A named workspace with its own panel layout.
#[derive(Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub name: String,
    pub panels: Vec<WorkspacePanel>,
    pub builtin: bool,
}

/// Manages all workspaces and tracks the active one.
#[derive(Clone, Serialize, Deserialize)]
pub struct WorkspaceManager {
    pub workspaces: Vec<Workspace>,
    pub active_idx: usize,
    #[serde(skip)]
    pub edit_mode: bool,
}

impl WorkspaceManager {
    pub fn active(&self) -> &Workspace {
        &self.workspaces[self.active_idx.min(self.workspaces.len().saturating_sub(1))]
    }
    pub fn active_mut(&mut self) -> &mut Workspace {
        let idx = self.active_idx.min(self.workspaces.len().saturating_sub(1));
        &mut self.workspaces[idx]
    }
    pub fn save(&self) {
        let Some(home) = dirs::home_dir() else { return };
        let dir = home.join(".azra");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("workspaces.json");
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
    pub fn load() -> Self {
        let Some(home) = dirs::home_dir() else { return Self::defaults() };
        let path = home.join(".azra").join("workspaces.json");
        match std::fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_else(|_| Self::defaults()),
            Err(_) => Self::defaults(),
        }
    }
    pub fn defaults() -> Self {
        Self {
            workspaces: vec![
                Workspace::sdf_modeling(),
                Workspace::sculpting(),
                Workspace::poly_modeling(),
                Workspace::nurbs(),
            ],
            active_idx: 0,
            edit_mode: false,
        }
    }
}

impl Workspace {
    pub fn sdf_modeling() -> Self {
        use ToolButtonId::*;
        Workspace {
            name: "SDF Modeling".into(), builtin: true,
            panels: vec![WorkspacePanel {
                id: "tools_sdf".into(), name: "Tools".into(),
                dock: PanelDock::Left, width: 52.0, collapsed: false, builtin_default: true,
                buttons: vec![
                    Label(0), ModeObject, ModeSculpt, ModeFace,
                    Separator,
                    Label(1), ShapeSphere, ShapeBox, ShapeCylinder, ShapeTorus,
                    ShapeCone, ShapeCapsule, ShapeStair, ShapeSpiral,
                    ShapePolyline, ShapeSpine, ToolBoolCut,
                    Separator,
                    Label(2), OpSubtract, OpUnion, OpIntersect, OpSlice,
                    Separator,
                    Label(3), GizmoMove, GizmoRotate, GizmoScale,
                    Separator,
                    ConvertToClay,
                ],
            }],
        }
    }
    pub fn sculpting() -> Self {
        use ToolButtonId::*;
        Workspace {
            name: "Sculpting".into(), builtin: true,
            panels: vec![WorkspacePanel {
                id: "tools_sculpt".into(), name: "Tools".into(),
                dock: PanelDock::Left, width: 52.0, collapsed: false, builtin_default: true,
                buttons: vec![
                    Label(0), ModeObject, ModeSculpt,
                    Separator,
                    Label(4), BrushAdd, BrushCarve, BrushSmooth, BrushFlatten, BrushInflate,
                    BrushPinch, BrushClay, BrushCrease, BrushTrim,
                ],
            }],
        }
    }
    pub fn poly_modeling() -> Self {
        use ToolButtonId::*;
        Workspace {
            name: "Poly Modeling".into(), builtin: true,
            panels: vec![WorkspacePanel {
                id: "tools_poly".into(), name: "Tools".into(),
                dock: PanelDock::Left, width: 52.0, collapsed: false, builtin_default: true,
                buttons: vec![
                    Label(0), ModeObject,
                    Separator,
                    Label(0), PolyModeObj, PolyModeVtx, PolyModeEdg, PolyModeFce,
                    Separator,
                    Label(5), PolyExtrude, PolyBevel, PolyLoopCut, PolyKnife, PolyMerge, PolyInset,
                    Separator,
                    Label(6), PolyCreateBox, PolyCreatePlane, PolyCreateSphere,
                    PolyCreateCylinder, PolyCreateTorus, PolyCreateCone,
                    Separator,
                    Label(3), GizmoMove, GizmoRotate, GizmoScale,
                ],
            }],
        }
    }
    pub fn nurbs() -> Self {
        use ToolButtonId::*;
        Workspace {
            name: "NURBS".into(), builtin: true,
            panels: vec![WorkspacePanel {
                id: "tools_nurbs".into(), name: "Tools".into(),
                dock: PanelDock::Left, width: 52.0, collapsed: false, builtin_default: true,
                buttons: vec![
                    Label(0), ModeObject,
                    Separator,
                    Label(0), NurbsModeObj, NurbsModeCv, NurbsModeSrf,
                    Separator,
                    Label(5), NurbsKnotU, NurbsKnotV, NurbsWeight, NurbsLoft, NurbsRevolve,
                    Separator,
                    Label(6), NurbsCreatePlane, NurbsCreateCylinder, NurbsCreateSphere, NurbsCreateTorus,
                    Separator,
                    Label(3), GizmoMove, GizmoRotate, GizmoScale,
                ],
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DATA-LOSS GATE (2026-07-14). Retiring a `ToolButtonId` must cost the user that button
    /// and nothing else.
    ///
    /// Before `buttons_skip_unknown`, `WorkspaceManager::load` fell back to `defaults()` on ANY
    /// parse error, so one unknown button string wiped every workspace in
    /// `~/.azra/workspaces.json` — silently, then permanently once `save()` ran. Verified real:
    /// the owner's file (9509 bytes, 9 workspaces) has RENAMED builtins ("SDF", not the default
    /// "SDF Modeling") plus 5 custom workspaces, and references BrushAdd/BrushCarve/BrushSmooth/
    /// BrushFlatten/BrushInflate and ConvertToClay — all slated for retirement. `defaults()`
    /// cannot reconstruct a renamed builtin, so that data would have been unrecoverable.
    ///
    /// Asserts the tolerant path: unknown ids drop, EVERYTHING else survives byte-for-byte.
    #[test]
    fn retiring_a_tool_button_drops_the_button_not_the_users_workspaces() {
        // Shaped like the real file: a renamed builtin + a custom workspace. The two retired
        // ids stand in for buttons a FUTURE build removes (they must not be live variants here,
        // or the test proves nothing — that mistake caught itself on first run). They bracket a
        // live button so we also prove order is preserved across a drop.
        let json = r#"{
            "workspaces": [
                { "name": "SDF", "builtin": true, "panels": [
                    { "id": "tools", "name": "Tools",
                      "buttons": ["ModeObject", "BrushRetiredInAFutureBuild", "ShapeSphere", "AnotherGoneButton"],
                      "dock": "Left", "width": 150.0, "collapsed": false, "builtin_default": true }
                ]},
                { "name": "Custom 5", "builtin": false, "panels": [
                    { "id": "p2", "name": "Mine", "buttons": ["ShapeBox"],
                      "dock": "Right", "width": 200.0, "collapsed": true, "builtin_default": false }
                ]}
            ],
            "active_idx": 1
        }"#;

        let m: WorkspaceManager = serde_json::from_str(json)
            .expect("an unknown button id must NOT fail the document — the user's file is not garbage");

        // The user's structure survives intact.
        assert_eq!(m.workspaces.len(), 2, "workspaces were discarded");
        assert_eq!(m.workspaces[0].name, "SDF", "a RENAMED builtin was reset to defaults() — unrecoverable");
        assert_eq!(m.workspaces[1].name, "Custom 5", "a user's custom workspace was lost");
        assert_eq!(m.active_idx, 1, "active workspace was reset");
        assert_eq!(m.workspaces[1].panels[0].width, 200.0, "panel layout was lost");
        assert!(m.workspaces[1].panels[0].collapsed, "panel state was lost");

        // Only the unknown ids dropped; the live ones kept their order.
        let b = &m.workspaces[0].panels[0].buttons;
        assert_eq!(b, &[ToolButtonId::ModeObject, ToolButtonId::ShapeSphere],
            "known buttons must survive in order, unknown ones drop (got {b:?})");
    }

    /// A wholly unparseable file (not merely an unknown button) must still fall back cleanly
    /// rather than panic — `load()`'s `unwrap_or_else` stays correct for genuine garbage.
    #[test]
    fn genuinely_corrupt_json_still_falls_back_to_defaults() {
        assert!(serde_json::from_str::<WorkspaceManager>("{ this is not json").is_err());
        assert_eq!(WorkspaceManager::defaults().workspaces.len(), 4);
    }
}
