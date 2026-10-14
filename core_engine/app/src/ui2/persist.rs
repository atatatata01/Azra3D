//! UI v2 layout persistence — dock groups, zone widths/hidden flags, library
//! chrome, outliner columns/filters, workspace tab.
//!
//! Blob: `<config_dir>/Azra/ui2_layout.json` — same directory + JSON pattern as
//! the v1 `panel::UiLayout` (`ui_layout.json`). Versioned (`"v": 1`), fail-soft:
//! missing/unreadable/wrong-version file → `Ui2State::default()`, never a crash;
//! unknown panel/column/filter ids are skipped individually.
//!
//! Save is DEBOUNCED — a fingerprint of the layout snapshot is compared each v2
//! frame (`ui2_layout_autosave`, called from `ui2_draw`); the file is written
//! once the layout has been stable for `SAVE_DEBOUNCE` after the last change,
//! and flushed on app exit. Never per-frame file I/O.

use eframe::egui;
use crate::*;
use super::{DockSide, OlCol, OlKind, PanelKind, Ui2State, WorkspaceTab};

/// Current blob schema version.
const BLOB_VERSION: u32 = 1;
/// Write once the layout has been stable this long after the last change.
const SAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(1000);

// ── Stable string ids (survive enum renames; NOT the scene-file bincode) ─────
// Matches are exhaustive on purpose: adding a variant fails compile here,
// forcing the author to pick a stable id.

fn panel_kind_id(k: PanelKind) -> &'static str {
    match k {
        PanelKind::Outliner => "outliner",
        PanelKind::Properties => "properties",
        PanelKind::ToolsSdf => "tools_sdf",
        PanelKind::ToolsPoly => "tools_poly",
        PanelKind::ToolsSculpt => "tools_sculpt",
        PanelKind::ToolsNurbs => "tools_nurbs",
        PanelKind::ToolParams => "tool_params",
        PanelKind::Library => "library",
        PanelKind::Image => "image",
        PanelKind::History => "history",
        PanelKind::UvLayout => "uv_layout",
        PanelKind::NodeEditor => "node_editor",
        PanelKind::Channels => "channels",
        // Viewport shading-bar settings panels — on-disk contract, never rename.
        PanelKind::WireSettings => "settings_wire",
        PanelKind::SolidSettings => "settings_solid",
        PanelKind::MaterialSettings => "settings_material",
        PanelKind::PbrSettings => "settings_pbr",
        PanelKind::PtSettings => "settings_pt",
        PanelKind::RenderSettingsGlobal => "settings_render_global",
        PanelKind::MatCapSettings => "settings_matcap",
        PanelKind::EnvSettings => "settings_env",
    }
}
fn panel_kind_from_id(s: &str) -> Option<PanelKind> {
    PanelKind::all().into_iter().find(|k| panel_kind_id(*k) == s)
}

fn side_id(s: DockSide) -> &'static str {
    match s {
        DockSide::Left => "left",
        DockSide::Right => "right",
        DockSide::Top => "top",
        DockSide::Bottom => "bottom",
        DockSide::Float => "float",
    }
}
fn side_from_id(s: &str) -> Option<DockSide> {
    match s {
        "left" => Some(DockSide::Left),
        "right" => Some(DockSide::Right),
        "top" => Some(DockSide::Top),
        "bottom" => Some(DockSide::Bottom),
        "float" => Some(DockSide::Float),
        _ => None,
    }
}

fn workspace_id(w: WorkspaceTab) -> &'static str {
    match w {
        WorkspaceTab::Model => "model",
        WorkspaceTab::Sculpt => "sculpt",
        WorkspaceTab::Sdf => "sdf",
        WorkspaceTab::Uv => "uv",
        WorkspaceTab::Shade => "shade",
        WorkspaceTab::Animate => "animate",
        WorkspaceTab::Render => "render",
    }
}
fn workspace_from_id(s: &str) -> Option<WorkspaceTab> {
    WorkspaceTab::ALL.into_iter().find(|w| workspace_id(*w) == s)
}

fn ol_col_id(c: OlCol) -> &'static str {
    match c {
        OlCol::Mod => "mod",
        OlCol::Anim => "anim",
        OlCol::Tex => "tex",
        OlCol::Uv => "uv",
        OlCol::Manifold => "manifold",
    }
}
fn ol_col_from_id(s: &str) -> Option<OlCol> {
    OlCol::ALL.into_iter().find(|c| ol_col_id(*c) == s)
}

fn ol_kind_id(k: OlKind) -> &'static str {
    match k {
        OlKind::Bodies => "bodies",
        OlKind::Prims => "prims",
        OlKind::Lights => "lights",
        OlKind::Cameras => "cameras",
        OlKind::Meshes => "meshes",
        OlKind::Materials => "materials",
    }
}
fn ol_kind_from_id(s: &str) -> Option<OlKind> {
    OlKind::ALL.into_iter().find(|k| ol_kind_id(*k) == s)
}

// ── Blob schema ──────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize)]
struct GroupBlob {
    side: String,
    tabs: Vec<String>,
    active: usize,
    collapsed: bool,
    head_coll: bool,
    height: f32,
    pos: [f32; 2],
    size: [f32; 2],
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Ui2LayoutFile {
    v: u32,
    groups: Vec<GroupBlob>,
    left_width: f32,
    right_width: f32,
    left_hidden: bool,
    right_hidden: bool,
    library_open: bool,
    library_collapsed: bool,
    library_height: f32,
    library_col_w: f32,
    /// `None` = field absent in the file → keep defaults (empty is a valid choice).
    #[serde(default)]
    ol_cols: Option<Vec<String>>,
    /// Empty = show all (same meaning as the runtime set).
    #[serde(default)]
    ol_kinds: Vec<String>,
    #[serde(default)]
    workspace: Option<String>,
}

fn blob_path() -> Option<std::path::PathBuf> {
    // Same directory + pattern as v1 `panel::UiLayout` (ui_layout.json).
    Some(dirs::config_dir()?.join("Azra").join("ui2_layout.json"))
}

fn finite_or(v: f32, fallback: f32) -> f32 {
    if v.is_finite() { v } else { fallback }
}

impl Ui2LayoutFile {
    fn snapshot(s: &Ui2State) -> Self {
        // HashSet iteration order is random — sort so the fingerprint is stable
        // frame-to-frame (an unstable order would fake "changed" every frame).
        let mut ol_kinds: Vec<String> =
            s.ol_kinds.iter().map(|k| ol_kind_id(*k).to_string()).collect();
        ol_kinds.sort();
        Self {
            v: BLOB_VERSION,
            groups: s
                .dock
                .iter()
                .map(|g| GroupBlob {
                    side: side_id(g.side).into(),
                    tabs: g.tabs.iter().map(|t| panel_kind_id(*t).to_string()).collect(),
                    active: g.active,
                    collapsed: g.collapsed,
                    head_coll: g.head_coll,
                    height: g.height,
                    pos: [g.pos.x, g.pos.y],
                    size: [g.size.x, g.size.y],
                })
                .collect(),
            left_width: s.left_width,
            right_width: s.right_width,
            left_hidden: s.left_hidden,
            right_hidden: s.right_hidden,
            library_open: s.library.open,
            library_collapsed: s.library.collapsed,
            library_height: s.library.height,
            library_col_w: s.library.col_w,
            ol_cols: Some(s.ol_cols.iter().map(|c| ol_col_id(*c).to_string()).collect()),
            ol_kinds,
            workspace: Some(workspace_id(s.workspace).into()),
        }
    }

    /// Apply onto a default-constructed state. Every field is range-guarded so a
    /// hand-edited/garbage blob degrades to defaults instead of a broken shell.
    fn apply(&self, s: &mut Ui2State) {
        // Dock groups — rebuild only if at least one group survives id mapping,
        // otherwise keep the factory dock.
        let defaults = Ui2State::default();
        let mut groups = Vec::new();
        for gb in &self.groups {
            let Some(side) = side_from_id(&gb.side) else { continue };
            let tabs: Vec<PanelKind> =
                gb.tabs.iter().filter_map(|t| panel_kind_from_id(t)).collect();
            if tabs.is_empty() {
                continue;
            }
            groups.push((side, tabs, gb));
        }
        if !groups.is_empty() {
            s.dock.clear();
            s.next_id = 0;
            for (side, tabs, gb) in groups {
                let n = tabs.len();
                let mut g = s.mk(tabs, side);
                g.active = gb.active.min(n - 1);
                g.collapsed = gb.collapsed;
                g.head_coll = gb.head_coll;
                g.height = finite_or(gb.height, 0.0).max(0.0);
                g.pos = egui::pos2(
                    finite_or(gb.pos[0], g.pos.x),
                    finite_or(gb.pos[1], g.pos.y),
                );
                g.size = egui::vec2(
                    finite_or(gb.size[0], g.size.x).max(200.0),
                    finite_or(gb.size[1], g.size.y).max(140.0),
                );
                s.dock.push(g);
            }
        }
        s.left_width = finite_or(self.left_width, defaults.left_width)
            .clamp(ui_theme::RAIL_MIN, ui_theme::RAIL_MAX);
        s.right_width = finite_or(self.right_width, defaults.right_width)
            .clamp(ui_theme::DOCK_MIN, ui_theme::DOCK_MAX);
        s.left_hidden = self.left_hidden;
        s.right_hidden = self.right_hidden;
        s.library.open = self.library_open;
        s.library.collapsed = self.library_collapsed;
        s.library.height = finite_or(self.library_height, s.library.height).clamp(60.0, 2000.0);
        s.library.col_w = finite_or(self.library_col_w, s.library.col_w).clamp(80.0, 600.0);
        if let Some(cols) = &self.ol_cols {
            s.ol_cols = cols.iter().filter_map(|c| ol_col_from_id(c)).collect();
        }
        s.ol_kinds = self.ol_kinds.iter().filter_map(|k| ol_kind_from_id(k)).collect();
        if let Some(w) = self.workspace.as_deref().and_then(workspace_from_id) {
            s.workspace = w;
        }
    }

    fn read() -> Option<Self> {
        let json = std::fs::read_to_string(blob_path()?).ok()?;
        let file: Self = serde_json::from_str(&json).ok()?;
        (file.v == BLOB_VERSION).then_some(file)
    }

    fn write(&self) {
        let Some(path) = blob_path() else { return };
        if let Ok(json) = serde_json::to_string_pretty(self) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, json);
        }
    }
}

/// Fingerprint of the persisted subset of the layout. Cheap (a few hundred
/// bytes of JSON); used for change detection so the file is only written when
/// the layout actually changed and has settled.
fn layout_fingerprint(s: &Ui2State) -> u64 {
    use std::hash::{Hash, Hasher};
    let json = serde_json::to_string(&Ui2LayoutFile::snapshot(s)).unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    json.hash(&mut h);
    h.finish()
}

/// Build the startup `Ui2State`: defaults, then the persisted blob on top
/// (fail-soft). Called from `AzraApp::new` — BEFORE the first frame.
pub(crate) fn ui2_state_load() -> Ui2State {
    let mut s = Ui2State::default();
    if let Some(blob) = Ui2LayoutFile::read() {
        blob.apply(&mut s);
    }
    // Baseline = what's on disk (or defaults) so startup never triggers a write.
    s.layout_saved_fp = layout_fingerprint(&s);
    s
}

impl crate::AzraApp {
    /// Debounced layout autosave — call once per v2 frame (end of `ui2_draw`).
    /// Detects change via fingerprint (covers every mutation seam: dock Acts,
    /// edge resize, zone hide, library drag, workspace tab) and writes once the
    /// layout has been stable for `SAVE_DEBOUNCE`. Never writes every frame.
    pub(crate) fn ui2_layout_autosave(&mut self) {
        let fp = layout_fingerprint(&self.ui2);
        if fp == self.ui2.layout_saved_fp {
            self.ui2.layout_pending = None;
            return;
        }
        match self.ui2.layout_pending {
            Some((pending_fp, since)) if pending_fp == fp => {
                if since.elapsed() >= SAVE_DEBOUNCE {
                    Ui2LayoutFile::snapshot(&self.ui2).write();
                    self.ui2.layout_saved_fp = fp;
                    self.ui2.layout_pending = None;
                }
            }
            // New or still-moving change → (re)start the settle timer.
            _ => self.ui2.layout_pending = Some((fp, std::time::Instant::now())),
        }
    }

    /// Flush an unsaved layout change on app exit (change made < debounce ago).
    pub(crate) fn ui2_layout_flush_on_exit(&mut self) {
        let fp = layout_fingerprint(&self.ui2);
        if fp != self.ui2.layout_saved_fp {
            Ui2LayoutFile::snapshot(&self.ui2).write();
            self.ui2.layout_saved_fp = fp;
            self.ui2.layout_pending = None;
        }
    }

    /// View → Reset Default Layout (v2): restore layout fields to factory
    /// defaults, keep non-layout state (outliner tree/filter text, tool
    /// selection, library browsing state), and delete the blob. The layout
    /// fields persisted in the blob (dock, widths, hidden flags, library
    /// chrome, outliner columns/kind filters, workspace tab) ARE layout and
    /// reset with it.
    pub(crate) fn ui2_reset_layout(&mut self) {
        let old = std::mem::take(&mut self.ui2);
        let mut s = Ui2State::default();
        // Non-layout state carried over.
        s.ol_collapsed = old.ol_collapsed;
        s.ol_filter = old.ol_filter;
        s.ol_kb_row = old.ol_kb_row;
        s.ol_multi_rename = old.ol_multi_rename;
        s.ol_manifold_cache = old.ol_manifold_cache;
        s.tool_cat = old.tool_cat;
        s.active_tool = old.active_tool;
        s.active_pillar = old.active_pillar;
        s.tool_collapsed = old.tool_collapsed;
        s.ctx_space = old.ctx_space;
        s.ctx_pivot = old.ctx_pivot;
        s.ctx_snap = old.ctx_snap;
        s.snap_edge_perp = old.snap_edge_perp;
        s.snap_volume = old.snap_volume;
        s.snap_affect_move = old.snap_affect_move;
        s.snap_affect_rotate = old.snap_affect_rotate;
        s.snap_affect_scale = old.snap_affect_scale;
        s.snap_angle_lock = old.snap_angle_lock;
        s.shade_bar_variant = old.shade_bar_variant;
        // Library: keep browsing state (selection, folders, favourites…),
        // reset only the layout chrome to LibraryUi factory values.
        s.library = old.library;
        let lib_defaults = crate::library_panel::LibraryUi::default();
        s.library.open = lib_defaults.open;
        s.library.collapsed = lib_defaults.collapsed;
        s.library.height = lib_defaults.height;
        s.library.col_w = lib_defaults.col_w;
        // Delete the blob; baseline the fingerprint so the debounce does not
        // immediately rewrite it — the file stays absent until the next change.
        if let Some(path) = blob_path() {
            let _ = std::fs::remove_file(path);
        }
        s.layout_saved_fp = layout_fingerprint(&s);
        s.layout_pending = None;
        self.ui2 = s;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_dock_sides_round_trip_through_layout_ids() {
        for side in [DockSide::Top, DockSide::Bottom] {
            assert!(side_from_id(side_id(side)) == Some(side));
        }
    }

    #[test]
    fn node_editor_horizontal_dock_round_trips_through_snapshot() {
        let mut source = Ui2State::default();
        let mut group = source.mk(vec![PanelKind::NodeEditor], DockSide::Bottom);
        group.height = 320.0;
        source.dock.push(group);

        let snapshot = Ui2LayoutFile::snapshot(&source);
        let mut restored = Ui2State::default();
        snapshot.apply(&mut restored);

        assert!(restored.dock.iter().any(|group| {
            group.side == DockSide::Bottom
                && group.tabs == vec![PanelKind::NodeEditor]
                && (group.height - 320.0).abs() < f32::EPSILON
        }));
    }
}
