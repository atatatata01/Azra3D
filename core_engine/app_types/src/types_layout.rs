// UI Layout Configuration — extracted from types.rs
use serde::{Serialize, Deserialize};

// ==================================================================
// UI Layout Configuration (Phase 1)
// ==================================================================
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum PanelSide { Left, Right }

#[derive(Clone, Serialize, Deserialize)]
pub struct ToolbarConfig {
    pub sections: Vec<ToolbarSection>,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ToolbarSection {
    Mode, Shape, BooleanOp, GizmoMode, Brush, PolyTools, NurbsTools, Separator,
}

impl Default for ToolbarConfig {
    fn default() -> Self {
        Self { sections: vec![
            ToolbarSection::Mode, ToolbarSection::Separator,
            ToolbarSection::Shape, ToolbarSection::Separator,
            ToolbarSection::BooleanOp, ToolbarSection::Separator,
            ToolbarSection::GizmoMode,
        ]}
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ThemeConfig {
    pub dark_mode: bool,
    pub font_size: f32,
    pub accent_color: [u8; 3],
    pub panel_bg_alpha: u8,
    pub selection_color: [u8; 3],
    pub viewport_bg: [u8; 3],
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            dark_mode: true, font_size: 13.0,
            accent_color: [30, 136, 229], panel_bg_alpha: 245,
            selection_color: [0, 200, 255], viewport_bg: [30, 30, 30],
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct UiLayoutConfig {
    pub tool_panel_side: PanelSide,
    pub properties_panel_side: PanelSide,
    pub tool_panel_width: f32,
    pub tool_panel_width_range: (f32, f32),
    pub properties_panel_width: f32,
    pub header_height: f32,
    pub show_tool_panel: bool,
    pub show_properties_panel: bool,
    pub show_viewport_header: bool,
    pub show_content_library: bool,
    pub outliner_height_fraction: f32,
    /// Outliner lives in a floating egui::Window instead of the right panel stack.
    /// `#[serde(default)]` so pre-existing layout.json files still parse.
    #[serde(default)]
    pub outliner_detached: bool,
    pub active_preset: String,
    pub toolbar: ToolbarConfig,
    pub theme: ThemeConfig,
}

impl Default for UiLayoutConfig {
    fn default() -> Self {
        Self {
            tool_panel_side: PanelSide::Left,
            properties_panel_side: PanelSide::Right,
            tool_panel_width: 52.0,
            tool_panel_width_range: (42.0, 120.0),
            properties_panel_width: 280.0,
            header_height: 28.0,
            show_tool_panel: true,
            show_properties_panel: true,
            show_viewport_header: true,
            show_content_library: true,
            outliner_height_fraction: 0.45,
            outliner_detached: false,
            active_preset: "Default".into(),
            toolbar: ToolbarConfig::default(),
            theme: ThemeConfig::default(),
        }
    }
}

impl UiLayoutConfig {
    pub fn preset_minimal() -> Self {
        Self {
            show_tool_panel: false, properties_panel_width: 220.0,
            show_viewport_header: false, active_preset: "Minimal".into(),
            ..Self::default()
        }
    }
    pub fn preset_wide() -> Self {
        Self {
            tool_panel_width: 80.0, properties_panel_width: 360.0,
            active_preset: "Wide".into(), ..Self::default()
        }
    }
    pub fn save(&self) {
        let Some(home) = dirs::home_dir() else { return };
        let dir = home.join(".azra");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("layout.json");
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
    pub fn load() -> Self {
        let Some(home) = dirs::home_dir() else { return Self::default() };
        let path = home.join(".azra").join("layout.json");
        match std::fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }
}
