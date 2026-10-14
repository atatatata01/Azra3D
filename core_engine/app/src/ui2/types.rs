// Split from ui2 god file. Prefer <500 lines; shared chrome lives in ui2::chrome.
use eframe::egui;
use egui_phosphor::regular as ph;
use crate::*;
use core_math::scene_graph::{NodePayload, NodeId};

// ── Panel kinds ──────────────────────────────────────────────────────────────
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PanelKind {
    Outliner, Properties,
    ToolsSdf, ToolsPoly, ToolsSculpt, ToolsNurbs,   // the four tool-mode windows (chrome tabs)
    ToolParams, Library, Image, History, UvLayout, NodeEditor, Channels,
    // Viewport shading-bar settings panels (one per mode + global). Real
    // dockable/float/persist panels — the shading strip's RMB popover and the
    // docked pane share the same draw fn (render_settings.rs).
    WireSettings, SolidSettings, MaterialSettings, PbrSettings, PtSettings,
    RenderSettingsGlobal,
    // MatCap is its own shading-bar button (owner 2026-07-26): unlit baked-sphere
    // sculpt shading. Settings = matcap selection + solid/gradient bg + edge lines.
    MatCapSettings,
    // HDR / environment — not a shading mode. LMB on the bar toggles mute;
    // RMB opens this settings surface (load/clear, intensity, dual blur, …).
    EnvSettings,
}
impl PanelKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            PanelKind::Outliner => "Outliner", PanelKind::Properties => "Properties",
            PanelKind::ToolsSdf => "SDF", PanelKind::ToolsPoly => "Poly",
            PanelKind::ToolsSculpt => "Sculpt", PanelKind::ToolsNurbs => "NURBS",
            PanelKind::ToolParams => "Tool Parameters", // Live · Last · Selection · Active — sole home
            PanelKind::Library => "Library", PanelKind::Image => "Image",
            PanelKind::History => "History", PanelKind::UvLayout => "UV Layout",
            PanelKind::NodeEditor => "Node Editor", PanelKind::Channels => "Geometry Channels",
            PanelKind::WireSettings => "Wire", PanelKind::SolidSettings => "Solid",
            PanelKind::MaterialSettings => "Material", PanelKind::PbrSettings => "PBR",
            PanelKind::PtSettings => "Path Trace", PanelKind::RenderSettingsGlobal => "Overlays",
            PanelKind::MatCapSettings => "MatCap",
            PanelKind::EnvSettings => "HDR / Environment",
        }
    }
    /// If this is one of the tool-mode windows, which pillar's tools it hosts.
    pub(crate) fn tool_pillar(self) -> Option<ToolPillar> {
        match self {
            PanelKind::ToolsSdf => Some(ToolPillar::Sdf),
            PanelKind::ToolsPoly => Some(ToolPillar::Poly),
            PanelKind::ToolsSculpt => Some(ToolPillar::Sculpt),
            PanelKind::ToolsNurbs => Some(ToolPillar::Nurbs),
            _ => None,
        }
    }
    pub(crate) fn all() -> [PanelKind; 21] {
        [PanelKind::Outliner, PanelKind::Properties,
         PanelKind::ToolsSdf, PanelKind::ToolsPoly, PanelKind::ToolsSculpt, PanelKind::ToolsNurbs,
         PanelKind::ToolParams, PanelKind::Library, PanelKind::Image, PanelKind::History,
         PanelKind::UvLayout, PanelKind::NodeEditor, PanelKind::Channels,
         // Settings panels appended at END (order affects only the Add-Panel menu,
         // never persistence — that keys on stable string ids in persist.rs).
         PanelKind::WireSettings, PanelKind::SolidSettings, PanelKind::MaterialSettings,
         PanelKind::PbrSettings, PanelKind::PtSettings, PanelKind::RenderSettingsGlobal,
         PanelKind::MatCapSettings, PanelKind::EnvSettings]
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DockSide { Left, Right, Top, Bottom, Float }

// ── Tools palette taxonomy ─────────────────────────────────────────────────────
/// The four modelling pillars. Each is a chrome tab (PanelKind::Tools*); this drives the taxonomy.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolPillar { Sdf, Poly, Sculpt, Nurbs }
impl ToolPillar {
    /// Pillar accent colour (data-type hue) used for the tool icons + active category.
    pub(crate) fn accent(self) -> egui::Color32 {
        match self { ToolPillar::Sdf => ui_theme::SDF, ToolPillar::Poly => ui_theme::MESH,
                     ToolPillar::Sculpt => ui_theme::SCULPT_ORANGE, ToolPillar::Nurbs => ui_theme::ACCENT_LIGHT }
    }
}

/// One tool entry in the palette (mockup `.tp-tool`).
/// `ready=false` → dimmed / disabled. `key` = shortcut label (empty if none).
/// `variants=true` → corner wedge (long-press variants — chrome only for now).
pub(crate) struct ToolItem {
    pub(crate) name: &'static str,
    pub(crate) icon: &'static str,
    pub(crate) key: &'static str,
    pub(crate) ready: bool,
    pub(crate) variants: bool,
}
/// One-shot command under a category (mockup `.tp-cmd`) — never takes active bar.
pub(crate) struct ToolCmd {
    pub(crate) name: &'static str,
    pub(crate) key: &'static str,
    pub(crate) ready: bool,
}
/// A sub-category (side-rail tab): tools folder + commands folder (mockup P.cats).
pub(crate) struct ToolCatDef {
    pub(crate) name: &'static str,
    pub(crate) icon: &'static str,
    pub(crate) tools: &'static [ToolItem],
    pub(crate) cmds: &'static [ToolCmd],
}
/// Shorthand for a ready tool with no shortcut / variants.
pub(crate) const fn t(name: &'static str, icon: &'static str) -> ToolItem {
    ToolItem { name, icon, key: "", ready: true, variants: false }
}
pub(crate) const fn tk(name: &'static str, icon: &'static str, key: &'static str) -> ToolItem {
    ToolItem { name, icon, key, ready: true, variants: false }
}
pub(crate) const fn tw(name: &'static str, icon: &'static str) -> ToolItem {
    ToolItem { name, icon, key: "", ready: false, variants: false }
}
pub(crate) const fn c(name: &'static str, key: &'static str) -> ToolCmd {
    ToolCmd { name, key, ready: true }
}
pub(crate) const fn cw(name: &'static str, key: &'static str) -> ToolCmd {
    ToolCmd { name, key, ready: false }
}

// ── Group model ──────────────────────────────────────────────────────────────
/// One dock group's last live paint, as raw egui shapes.
///
/// A shape replay rather than a hand-written painter re-implementation per panel kind: it is
/// pixel-identical to what the panel last showed, it works for EVERY panel kind including ones
/// nobody wrote an idle painter for, and there is one implementation instead of one per panel.
#[derive(Clone)]
pub(crate) struct IdlePanelSnapshot {
    /// Body rect the shapes were captured in — replay translates by the delta, so a resized or
    /// moved dock still shows its content in the right place.
    pub(crate) rect: egui::Rect,
    pub(crate) shapes: Vec<egui::epaint::ClippedShape>,
    pub(crate) epoch: u64,
}

#[derive(Clone)]
pub(crate) struct PanelGroup {
    pub id: u64,
    pub tabs: Vec<PanelKind>,
    pub active: usize,
    /// Window collapse: body hidden, head (tab strip) remains (mockup `.pane.collapsed`).
    pub collapsed: bool,
    /// Header collapse: head becomes a 6px restore line (mockup `.pane.__headcoll`).
    pub head_coll: bool,
    pub side: DockSide,
    pub height: f32,
    pub pos: egui::Pos2,
    pub size: egui::Vec2,
}
impl PanelGroup {
    pub(crate) fn active_kind(&self) -> Option<PanelKind> { self.tabs.get(self.active).copied() }
}

pub(crate) struct Ui2State {
    pub dock: Vec<PanelGroup>,
    pub left_width: f32,
    pub right_width: f32,
    pub(crate) split_shift: bool,
    pub(crate) next_id: u64,
    pub(crate) left_rects: Vec<(u64, egui::Rect)>,
    pub(crate) right_rects: Vec<(u64, egui::Rect)>,
    pub(crate) top_rects: Vec<(u64, egui::Rect)>,
    pub(crate) bottom_rects: Vec<(u64, egui::Rect)>,
    /// Outliner: collapsed scene-graph node ids (by NodeId.0).
    pub(crate) ol_collapsed: std::collections::HashSet<u64>,
    pub(crate) ol_filter: String,
    /// Kind filter funnel: empty = show all; otherwise only listed kinds (plus ancestors).
    pub(crate) ol_kinds: std::collections::HashSet<OlKind>,
    /// Cached `ol_build()` rows — rebuilt on structural/filter/expand change only.
    /// Selection/active-body flags are patched in place each frame (cheap).
    /// At 10k bodies a full rebuild every frame was ~40+ ms of `lay=` (C-scale scoreboard).
    pub(crate) ol_rows_cache: Vec<crate::ui2::outliner::OlRow>,
    /// IDLE PANEL SNAPSHOTS — the last LIVE paint of each dock group, replayed while idle.
    ///
    /// Keyed by a SLOT id (see `idle::slot`): pane bodies key on group + active tab so switching
    /// tabs cannot replay the other tab's picture, and the chrome bars and tab strips get their
    /// own constants. `epoch` is the UI-content epoch the snapshot was taken at: when the epoch moves
    /// (selection, hierarchy, tool, poly edit) the snapshot is stale and the panel goes live for
    /// one frame to retake it. Without that, an idle Properties panel would keep showing the
    /// previous object's values after a viewport click.
    pub(crate) idle_snapshots: std::collections::HashMap<u64, IdlePanelSnapshot>,
    pub(crate) ol_rows_filter: String,
    pub(crate) ol_rows_kinds: std::collections::HashSet<OlKind>,
    pub(crate) ol_rows_collapsed: std::collections::HashSet<u64>,
    /// Keyboard focus row index into last ol_build list (None = follow selection).
    pub(crate) ol_kb_row: Option<usize>,
    /// Previous-frame: pointer was over the outliner pane body.
    /// Used by `handle_keyboard` to defer Space (cancel/deselect) to tree expand toggle.
    pub(crate) ol_pointer_over: bool,
    /// Multi-rename popup: base name while open (`None` = closed).
    pub(crate) ol_multi_rename: Option<String>,
    /// Manifold check cache: NodeId.0 → (fingerprint, is_manifold).
    /// Fingerprint = Arc ptr ⊕ topology counts (O(1); never content_signature).
    /// `is_manifold()` only re-runs when that identity moves.
    pub(crate) ol_manifold_cache: std::collections::HashMap<u64, (u64, bool)>,
    /// Outliner info columns, in display order. Right-click the Name header to configure.
    pub(crate) ol_cols: Vec<OlCol>,
    /// Tools panel: selected sub-category index (pillar comes from the active chrome tab).
    pub(crate) tool_cat: usize,
    /// Gate 4: last tool clicked in the Tools palette (drives Tool Parameters + row highlight).
    pub(crate) active_tool: String,
    /// Gate 4: pillar string for tool_params ("sdf"/"poly"/"sculpt"/"nurbs").
    pub(crate) active_pillar: String,
    /// Gate 7: Library bottom overlay chrome.
    pub library: crate::library_panel::LibraryUi,
    /// Viewport hole between side docks (library + Model live only here).
    /// Computed from real SidePanel rects — not raw available_rect alone (that
    /// drifted and let the library paint under Properties).
    pub viewport_hole: egui::Rect,
    /// Last frame side-column rects (screen space) for hole math + edge hits.
    pub(crate) left_panel_rect: egui::Rect,
    pub(crate) right_panel_rect: egui::Rect,
    pub(crate) top_panel_rect: egui::Rect,
    pub(crate) bottom_panel_rect: egui::Rect,
    /// Gate 6: Shift+A add-menu open + anchor (screen pos).
    pub(crate) add_menu_open: bool,
    pub(crate) add_menu_pos: egui::Pos2,
    /// Gate 8: viewport split (mockup corner-drag). `None` = single full view.
    pub vp_split: Option<VpSplit>,
    /// Independent orbit camera for View B while split is active.
    /// View A always uses `viewport.camera`. Cleared on join.
    pub(crate) camera_b: Option<CameraState>,
    /// Active split pane: `0` = View A (`viewport.camera`), `1` = View B (`camera_b`).
    /// Click / hover-drag routes orbit and tools to this pane.
    pub(crate) split_active_pane: u8,
    /// Corner-split gesture: (corner_index, pointer start screen pos).
    pub(crate) split_drag: Option<(usize, egui::Pos2)>,
    /// App-bar workspace tab (mockup `#tabs` Model…Render). Chrome selection only for now.
    pub(crate) workspace: WorkspaceTab,
    /// Ctx-bar transform space / pivot / snap labels (popovers still stub content).
    pub(crate) ctx_space: &'static str,
    pub(crate) ctx_pivot: &'static str,
    pub(crate) ctx_snap: &'static str,
    /// Snap menu: poly-only targets not yet on ViewportState (UI + future engine).
    pub(crate) snap_edge_perp: bool,
    pub(crate) snap_volume: bool,
    /// Snap menu: which transform channels are affected (mockup Affects row).
    pub(crate) snap_affect_move: bool,
    pub(crate) snap_affect_rotate: bool,
    pub(crate) snap_affect_scale: bool,
    /// Snap OPTIONS → Angle lock: free Move/Grab direction snaps to Rotation step (extrude edge at N°).
    pub(crate) snap_angle_lock: bool,
    /// Snap OPTIONS → Align Rotation to Target (Blender): on geometry/surface snap with a known
    /// normal, rotate so local +Y aligns to that normal (Y-up world / scatter parity).
    pub(crate) snap_align_rotation: bool,
    /// Collapsed folder keys in the tools list (`"sdf:Shapes"`, `"sdf:Shapes:__cmds"`).
    pub(crate) tool_collapsed: std::collections::HashSet<String>,
    /// Entire left/right dock zone hidden → 6px restore edge (mockup `__lhide` / `__rhide`).
    pub(crate) left_hidden: bool,
    pub(crate) right_hidden: bool,
    /// Per-side drag-past-min resistance (mockup SLOP=60).
    pub(crate) left_hide_slop: f32,
    pub(crate) right_hide_slop: f32,
    /// Pointer-driven L/R edge resize in progress: `Some(true)` = left, `Some(false)` = right.
    /// Not egui Area — raw pointer so gizmo/viewport cannot steal the drag.
    pub(crate) edge_drag: Option<bool>,
    /// Tab reorder drag: (group id, source tab index).
    pub(crate) tab_drag: Option<(u64, usize)>,
    /// Single-tab ⋮ float peel: (group id, tab index).
    pub(crate) tab_float_drag: Option<(u64, usize)>,
    /// VESTIGIAL (owner 2026-07-26): the Material/PBR Pbr-pair was merged into one
    /// PBR button, so this no longer disambiguates anything. Field retained only to
    /// avoid churning the ui2-state persist migration ABI; no reader remains.
    pub(crate) shade_bar_variant: u8,
    /// Viewport shading bar: which settings popover is open (in-viewport, top-right).
    /// `None` = closed. Session-only — NOT persisted. The docked-panel path
    /// (`ui2_reveal_panel`) is separate and preserved for the detach button.
    pub(crate) shade_popover: Option<PanelKind>,
    /// Sculpt tool-header popover (in-viewport top strip). Session-only.
    pub(crate) sculpt_header_popover: Option<SculptHeaderPopover>,
    /// Layout persistence (ui2::persist): fingerprint of the last snapshot
    /// written to (or loaded from) `ui2_layout.json`.
    pub(crate) layout_saved_fp: u64,
    /// Pending debounced save: (fingerprint of the changed layout, first seen).
    /// Written once stable for the debounce window; reset when fp moves again.
    pub(crate) layout_pending: Option<(u64, std::time::Instant)>,
    /// Working geometry channel for the Geometry Channels panel (select-by-channel).
    /// Session-only — not part of layout persistence.
    pub(crate) geo_channel_work: core_math::geometry_channel::GeometryChannel,
}

/// Mockup app-bar workspace tabs (`data-w`). Order and labels are fixed to the HTML oracle.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceTab {
    Model, Sculpt, Sdf, Uv, Shade, Animate, Render,
}
impl WorkspaceTab {
    pub(crate) const ALL: [WorkspaceTab; 7] = [
        WorkspaceTab::Model, WorkspaceTab::Sculpt, WorkspaceTab::Sdf, WorkspaceTab::Uv,
        WorkspaceTab::Shade, WorkspaceTab::Animate, WorkspaceTab::Render,
    ];
    pub(crate) fn label(self) -> &'static str {
        match self {
            WorkspaceTab::Model => "Model",
            WorkspaceTab::Sculpt => "Sculpt",
            WorkspaceTab::Sdf => "SDF",
            WorkspaceTab::Uv => "UV",
            WorkspaceTab::Shade => "Shade",
            WorkspaceTab::Animate => "Animate",
            WorkspaceTab::Render => "Render",
        }
    }
    /// Mockup shows a violet activity dot on inactive SDF (live pillar indicator).
    pub(crate) fn show_dot(self) -> bool { matches!(self, WorkspaceTab::Sdf) }
}

/// In-viewport sculpt tool-header popovers (Blender VIEW3D_PT_tools_brush_*).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SculptHeaderPopover {
    Brush,
    Texture,
    Stroke,
    /// Dyntopo settings (Blender VIEW3D_PT_sculpt_dyntopo analog) — detailing mode,
    /// resolution, and the live Remesh modifier's extract knobs.
    Dyntopo,
}

impl SculptHeaderPopover {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Brush => "Brush Settings",
            Self::Texture => "Texture",
            Self::Stroke => "Stroke",
            Self::Dyntopo => "Dyntopo",
        }
    }
    pub(crate) fn ord(self) -> u8 {
        match self {
            Self::Brush => 0,
            Self::Texture => 1,
            Self::Stroke => 2,
            Self::Dyntopo => 3,
        }
    }
}

/// Binary viewport split. `ratio` is the fraction of the first pane (0.15..0.85).
#[derive(Clone, Copy)]
pub struct VpSplit {
    pub vertical_seam: bool, // true = left|right, false = top/bottom
    pub ratio: f32,
}

impl VpSplit {
    /// Pane rects inside the scene rect: (View A, View B).
    pub fn pane_rects(self, full: egui::Rect) -> (egui::Rect, egui::Rect) {
        let ratio = self.ratio.clamp(0.15, 0.85);
        if self.vertical_seam {
            let x = full.left() + full.width() * ratio;
            (
                egui::Rect::from_min_max(full.left_top(), egui::pos2(x, full.bottom())),
                egui::Rect::from_min_max(egui::pos2(x, full.top()), full.right_bottom()),
            )
        } else {
            let y = full.top() + full.height() * ratio;
            (
                egui::Rect::from_min_max(full.left_top(), egui::pos2(full.right(), y)),
                egui::Rect::from_min_max(egui::pos2(full.left(), y), full.right_bottom()),
            )
        }
    }
}
impl Ui2State {
    pub(crate) fn mk(&mut self, tabs: Vec<PanelKind>, side: DockSide) -> PanelGroup {
        self.next_id += 1;
        PanelGroup {
            id: self.next_id, tabs, active: 0, collapsed: false, head_coll: false, side,
            height: 0.0, pos: egui::pos2(220.0, 120.0), size: egui::vec2(320.0, 400.0),
        }
    }
}
impl Default for Ui2State {
    fn default() -> Self {
        let mut s = Self {
            dock: Vec::new(),
            // Mockup body: `var(--railw,200px) 5px 1fr 5px var(--dockw,300px)`.
            left_width: ui_theme::RAIL_DEFAULT,
            right_width: ui_theme::DOCK_DEFAULT,
            split_shift: false, next_id: 0,
            left_rects: Vec::new(), right_rects: Vec::new(),
            top_rects: Vec::new(), bottom_rects: Vec::new(),
            ol_collapsed: std::collections::HashSet::new(), ol_filter: String::new(),
            ol_kinds: std::collections::HashSet::new(),
            ol_rows_cache: Vec::new(),
            idle_snapshots: std::collections::HashMap::new(),
            ol_rows_filter: String::new(),
            ol_rows_kinds: std::collections::HashSet::new(),
            ol_rows_collapsed: std::collections::HashSet::new(),
            ol_kb_row: None,
            ol_pointer_over: false,
            ol_multi_rename: None,
            ol_manifold_cache: std::collections::HashMap::new(),
            // Manifold fills for poly bodies; Anim stays n/a (no tracks yet).
            ol_cols: vec![OlCol::Mod, OlCol::Tex, OlCol::Uv],
            tool_cat: 0,
            active_tool: "Box".into(),
            active_pillar: "sdf".into(),
            library: crate::library_panel::LibraryUi::default(),
            viewport_hole: egui::Rect::NOTHING,
            left_panel_rect: egui::Rect::NOTHING,
            right_panel_rect: egui::Rect::NOTHING,
            top_panel_rect: egui::Rect::NOTHING,
            bottom_panel_rect: egui::Rect::NOTHING,
            add_menu_open: false,
            add_menu_pos: egui::pos2(400.0, 300.0),
            vp_split: None,
            camera_b: None,
            split_active_pane: 0,
            split_drag: None,
            workspace: WorkspaceTab::Model,
            // Mockup ctx pills: Global · Bounding Center · Snap
            ctx_space: "Global",
            ctx_pivot: "Bounding Center",
            ctx_snap: "Snap",
            snap_edge_perp: false,
            snap_volume: false,
            snap_affect_move: true,
            snap_affect_rotate: true,
            snap_affect_scale: true,
            snap_angle_lock: true,
            snap_align_rotation: false,
            tool_collapsed: std::collections::HashSet::new(),
            left_hidden: false,
            right_hidden: false,
            left_hide_slop: 0.0,
            right_hide_slop: 0.0,
            edge_drag: None,
            tab_drag: None,
            tab_float_drag: None,
            shade_bar_variant: 1, // default highlight = PBR (full post-FX)
            shade_popover: None,
            sculpt_header_popover: None,
            layout_saved_fp: 0,
            layout_pending: None,
            geo_channel_work: core_math::geometry_channel::GeometryChannel::default(),
        };
        // Default dock: Tools + Tool Params (left), Outliner + Properties (right).
        // Geometry Channels is intentionally NOT here — open from Add Panel / View menu.
        let tools = s.mk(vec![PanelKind::ToolsSdf, PanelKind::ToolsPoly,
                              PanelKind::ToolsSculpt, PanelKind::ToolsNurbs], DockSide::Left);
        let tparm = s.mk(vec![PanelKind::ToolParams], DockSide::Left);
        let outl  = s.mk(vec![PanelKind::Outliner], DockSide::Right);
        let props = s.mk(vec![PanelKind::Properties], DockSide::Right);
        s.dock = vec![tools, tparm, outl, props];
        s
    }
}

impl Ui2State {
    /// Human-readable UI v2 layout dump for agent/owner debug (panel size, position, dock state).
    /// Includes mockup oracle targets so live numbers can be compared side-by-side.
    pub(crate) fn layout_debug_report(&self, screen: egui::Rect) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(out, "==== AZRA UI2 LAYOUT DUMP ====");
        let _ = writeln!(out, "oracle=docs/UI/azra_app_mockup.html");
        let _ = writeln!(
            out,
            "mockup_grid=.body: var(--railw,200px) 5px 1fr 5px var(--dockw,300px)"
        );
        let _ = writeln!(
            out,
            "mockup_chrome: appbar=44 ctx=36 status=24 header=34 seam=5"
        );
        let _ = writeln!(
            out,
            "tokens: RAIL_DEFAULT={} DOCK_DEFAULT={} RAIL_MIN={} RAIL_MAX={} DOCK_MIN={} DOCK_MAX={}",
            ui_theme::RAIL_DEFAULT,
            ui_theme::DOCK_DEFAULT,
            ui_theme::RAIL_MIN,
            ui_theme::RAIL_MAX,
            ui_theme::DOCK_MIN,
            ui_theme::DOCK_MAX
        );
        let _ = writeln!(
            out,
            "screen: x={:.1} y={:.1} w={:.1} h={:.1}",
            screen.min.x,
            screen.min.y,
            screen.width(),
            screen.height()
        );
        let vh = self.viewport_hole;
        let _ = writeln!(
            out,
            "viewport_hole: x={:.1} y={:.1} w={:.1} h={:.1}",
            vh.min.x,
            vh.min.y,
            vh.width(),
            vh.height()
        );
        let _ = writeln!(
            out,
            "dock_widths: left={:.1} right={:.1}  (mockup defaults 200 / 300)",
            self.left_width, self.right_width
        );
        let _ = writeln!(
            out,
            "zone_hidden: left={} right={}  (mockup __lhide / __rhide → 6px restore)",
            self.left_hidden, self.right_hidden
        );
        let _ = writeln!(
            out,
            "workspace_tab={}  tool={}/{}  tool_cat={}",
            self.workspace.label(),
            self.active_pillar,
            self.active_tool,
            self.tool_cat
        );
        let _ = writeln!(
            out,
            "library: open={} collapsed={} preferred_h={:.1}  (mockup --libh default 180; zone-hide vs pane-coll)",
            self.library.open, self.library.collapsed, self.library.height
        );
        if let Some(sp) = self.vp_split {
            let _ = writeln!(
                out,
                "vp_split: vertical_seam={} ratio={:.3}",
                sp.vertical_seam, sp.ratio
            );
        } else {
            let _ = writeln!(out, "vp_split: none");
        }

        let _ = writeln!(out, "\n-- measured zone rects (after last frame) --");
        for (id, r) in &self.left_rects {
            let _ = writeln!(
                out,
                "  left_rect  id={}  x={:.1} y={:.1} w={:.1} h={:.1}",
                id,
                r.min.x,
                r.min.y,
                r.width(),
                r.height()
            );
        }
        for (id, r) in &self.right_rects {
            let _ = writeln!(
                out,
                "  right_rect id={}  x={:.1} y={:.1} w={:.1} h={:.1}",
                id,
                r.min.x,
                r.min.y,
                r.width(),
                r.height()
            );
        }

        let _ = writeln!(out, "\n-- panel groups (state) --");
        for (i, g) in self.dock.iter().enumerate() {
            let side = match g.side {
                DockSide::Left => "Left",
                DockSide::Right => "Right",
                DockSide::Top => "Top",
                DockSide::Bottom => "Bottom",
                DockSide::Float => "Float",
            };
            let tabs: Vec<&str> = g.tabs.iter().map(|t| t.label()).collect();
            let active = g.active_kind().map(|k| k.label()).unwrap_or("(none)");
            let _ = writeln!(
                out,
                "[{i}] id={} side={side} collapsed={} head_coll={} height={:.1}",
                g.id, g.collapsed, g.head_coll, g.height
            );
            let _ = writeln!(
                out,
                "     tabs=[{}] active={} active_idx={}",
                tabs.join(", "),
                active,
                g.active
            );
            let _ = writeln!(
                out,
                "     pos=({:.1},{:.1}) size=({:.1}x{:.1})  (float state; docked uses zone rect)",
                g.pos.x, g.pos.y, g.size.x, g.size.y
            );
        }

        // Quick mockup parity flags (not exhaustive — owner still signs off visually).
        let _ = writeln!(out, "\n-- quick parity vs mockup defaults --");
        let rail_ok = (self.left_width - ui_theme::RAIL_DEFAULT).abs() < 0.5;
        let dock_ok = (self.right_width - ui_theme::DOCK_DEFAULT).abs() < 0.5;
        let _ = writeln!(
            out,
            "left_width == 200 default: {}  (live={:.1})",
            if rail_ok { "YES" } else { "NO — resized or drifted" },
            self.left_width
        );
        let _ = writeln!(
            out,
            "right_width == 300 default: {}  (live={:.1})",
            if dock_ok { "YES" } else { "NO — resized or drifted" },
            self.right_width
        );
        let left_n = self.dock.iter().filter(|g| g.side == DockSide::Left).count();
        let right_n = self.dock.iter().filter(|g| g.side == DockSide::Right).count();
        let top_n = self.dock.iter().filter(|g| g.side == DockSide::Top).count();
        let bottom_n = self.dock.iter().filter(|g| g.side == DockSide::Bottom).count();
        let float_n = self.dock.iter().filter(|g| g.side == DockSide::Float).count();
        let _ = writeln!(
            out,
            "groups: left={left_n} right={right_n} top={top_n} bottom={bottom_n} float={float_n}  (default layout: L2 R2 F0)"
        );
        out
    }
}

// ── Actions ──────────────────────────────────────────────────────────────────
pub(crate) enum Act {
    Collapse(usize),
    /// Toggle header-only collapse (6px restore line).
    HeadCollapse(usize),
    Close(usize),
    InsertGroupAfter(usize),
    AddTab(usize, PanelKind),
    SetActive(usize, usize),
    /// Reorder tabs within a group (mockup drag-to-reorder with slide).
    ReorderTab { gi: usize, from: usize, to: usize },
    /// Close a single tab; removes the group if it was the last tab.
    CloseTab { gi: usize, ti: usize },
    /// Close every tab except `ti` in the group.
    CloseOtherTabs { gi: usize, ti: usize },
    /// Peel one tab into a floating single-tab panel at `pos`.
    FloatTab { group_id: u64, ti: usize, pos: egui::Pos2 },
    /// Live drag of a single tab’s ⋮ grip (ghost + release to float).
    TabFloatDrag {
        group_id: u64,
        ti: usize,
        pos: egui::Pos2,
        delta: egui::Vec2,
        released: bool,
    },
    /// Float / re-dock a group.
    ToggleFloat(usize),
    /// Move a group to an explicit dock edge.
    MoveToSide { group_id: u64, side: DockSide },
    /// Hide entire left or right dock zone.
    HideZone(DockSide),
    GripDrag { id: u64, floating: bool, pos: egui::Pos2, delta: egui::Vec2, released: bool },
    ResizeFloat(u64, egui::Vec2, egui::Vec2),
    Remove(u64),
}

// HEADER_H / PANE_MIN_H live in `ui2::chrome` — single source for all hosts.

// ── Outliner info columns (owned here so Ui2State can reference them) ─────────
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub(crate) enum OlCol { Mod = 0, Anim = 1, Tex = 2, Uv = 3, Manifold = 4 }
impl OlCol {
    pub(crate) const ALL: [OlCol; 5] = [OlCol::Mod, OlCol::Anim, OlCol::Tex, OlCol::Uv, OlCol::Manifold];
    pub(crate) fn label(self) -> &'static str {
        match self {
            OlCol::Mod => "Modifiers",
            OlCol::Anim => "Anim (n/a)",
            OlCol::Tex => "Textures",
            OlCol::Uv => "UV Sets",
            OlCol::Manifold => "Manifold",
        }
    }
    pub(crate) fn tip(self) -> &'static str {
        match self {
            OlCol::Mod => "Modifier count on poly bodies / prims",
            OlCol::Anim => "No animation tracks in AZRA yet — column stays empty",
            OlCol::Tex => "Texture map count",
            OlCol::Uv => "UV layer present on poly body",
            OlCol::Manifold => "Poly mesh manifold check (warning if broken)",
        }
    }
    pub(crate) fn icon(self) -> &'static str {
        match self {
            OlCol::Mod => ph::WRENCH, OlCol::Anim => ph::KEY, OlCol::Tex => ph::IMAGE,
            OlCol::Uv => ph::GRID_FOUR, OlCol::Manifold => ph::WARNING,
        }
    }
}

/// Outliner type-filter kinds (funnel menu). Empty set = show everything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum OlKind {
    Bodies,
    Prims,
    Lights,
    Cameras,
    Meshes,
    Materials,
}

impl OlKind {
    pub(crate) const ALL: [OlKind; 6] = [
        OlKind::Bodies,
        OlKind::Prims,
        OlKind::Lights,
        OlKind::Cameras,
        OlKind::Meshes,
        OlKind::Materials,
    ];
    pub(crate) fn label(self) -> &'static str {
        match self {
            OlKind::Bodies => "SDF Bodies",
            OlKind::Prims => "Primitives",
            OlKind::Lights => "Lights",
            OlKind::Cameras => "Cameras",
            OlKind::Meshes => "Meshes / Poly / Voxel",
            OlKind::Materials => "Materials",
        }
    }
    pub(crate) fn icon(self) -> &'static str {
        match self {
            OlKind::Bodies => ph::CIRCLES_THREE,
            OlKind::Prims => ph::CUBE,
            OlKind::Lights => ph::LIGHTBULB,
            OlKind::Cameras => ph::VIDEO_CAMERA,
            OlKind::Meshes => ph::POLYGON,
            OlKind::Materials => ph::PAINT_BRUSH,
        }
    }
}

