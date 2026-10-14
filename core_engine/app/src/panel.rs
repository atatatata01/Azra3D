//! Panel registry — tracks which panels are visible, their dock zone, and order.
//! Serialized to JSON for persistent UI customization.

/// Unique identifier for each panel in the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) enum PanelId {
    Outliner,
    Properties,
    MaterialEditor,
    ModifierStack,
    Render,
    LightEnv,
    Profiler,
    Compositor,
    AgentBar,
    ToolProperties,
    ViewportHeader,
    LeftToolStrip,
    DeveloperTweaks,
}

/// Where a panel can be docked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum DockZone {
    Left,
    Right,
    Bottom,
    Floating,
}

/// Saved state for one panel.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct PanelState {
    pub id: PanelId,
    pub visible: bool,
    pub dock: DockZone,
    pub order: u32,
    pub collapsed: bool,
}

/// The full panel layout — serialized to JSON.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct UiLayout {
    pub panels: Vec<PanelState>,
    pub ui_edit_mode: bool,
}

impl Default for UiLayout {
    fn default() -> Self {
        Self {
            panels: vec![
                PanelState { id: PanelId::Outliner,        visible: true,  dock: DockZone::Right,    order: 0,  collapsed: false },
                PanelState { id: PanelId::Properties,      visible: true,  dock: DockZone::Right,    order: 1,  collapsed: false },
                // MaterialEditor: registry-only phantom (no draw site ever existed) — dropped from
                // the default layout and hidden in the Panel List. The enum variant stays for
                // serde compat with existing ui_layout.json files. Material editing lives in
                // Properties (select a material node in the outliner's Materials section).
                PanelState { id: PanelId::ModifierStack,   visible: true,  dock: DockZone::Right,    order: 3,  collapsed: true  },
                PanelState { id: PanelId::Render,          visible: true,  dock: DockZone::Right,    order: 10, collapsed: true  },
                PanelState { id: PanelId::LightEnv,        visible: true,  dock: DockZone::Right,    order: 11, collapsed: true  },
                PanelState { id: PanelId::Profiler,        visible: false, dock: DockZone::Right,    order: 12, collapsed: true  },
                PanelState { id: PanelId::Compositor,      visible: false, dock: DockZone::Right,    order: 13, collapsed: true  },
                PanelState { id: PanelId::AgentBar,        visible: true,  dock: DockZone::Bottom,   order: 0,  collapsed: false },
                PanelState { id: PanelId::ToolProperties,  visible: true,  dock: DockZone::Left,     order: 10, collapsed: false },
                PanelState { id: PanelId::ViewportHeader,  visible: true,  dock: DockZone::Left,     order: 0,  collapsed: false },
                PanelState { id: PanelId::LeftToolStrip,   visible: true,  dock: DockZone::Left,     order: 1,  collapsed: false },
                PanelState { id: PanelId::DeveloperTweaks, visible: false, dock: DockZone::Floating, order: 0,  collapsed: false },
            ],
            ui_edit_mode: false,
        }
    }
}

impl UiLayout {
    pub fn panel(&self, id: PanelId) -> Option<&PanelState> {
        self.panels.iter().find(|p| p.id == id)
    }

    #[allow(dead_code)] // FUTURE: panel docking system
    pub fn panel_mut(&mut self, id: PanelId) -> Option<&mut PanelState> {
        self.panels.iter_mut().find(|p| p.id == id)
    }

    pub fn is_visible(&self, id: PanelId) -> bool {
        self.panel(id).map(|p| p.visible).unwrap_or(false)
    }

    #[allow(dead_code)] // FUTURE: panel docking system
    pub fn panels_in_zone(&self, zone: DockZone) -> Vec<PanelId> {
        let mut panels: Vec<_> = self.panels.iter()
            .filter(|p| p.dock == zone && p.visible)
            .collect();
        panels.sort_by_key(|p| p.order);
        panels.iter().map(|p| p.id).collect()
    }

    pub fn save_to_file(&self) {
        let path = dirs::config_dir()
            .unwrap_or_default()
            .join("Azra")
            .join("ui_layout.json");
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::create_dir_all(path.parent().unwrap());
            let _ = std::fs::write(&path, json);
        }
    }

    pub fn load_from_file() -> Option<Self> {
        let path = dirs::config_dir()?
            .join("Azra")
            .join("ui_layout.json");
        let json = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&json).ok()
    }
}

impl PanelId {
    pub fn label(&self) -> &'static str {
        match self {
            PanelId::Outliner => "Outliner",
            PanelId::Properties => "Properties",
            PanelId::MaterialEditor => "Material Editor",
            PanelId::ModifierStack => "Modifier Stack",
            PanelId::Render => "Render Settings",
            PanelId::LightEnv => "Light & Environment",
            PanelId::Profiler => "Profiler",
            PanelId::Compositor => "Compositor",
            PanelId::AgentBar => "Agent Bar",
            PanelId::ToolProperties => "Tool Properties",
            PanelId::ViewportHeader => "Viewport Header",
            PanelId::LeftToolStrip => "Left Tool Strip",
            PanelId::DeveloperTweaks => "Developer Tweaks",
        }
    }
}

impl crate::AzraApp {
    /// Draw the Panel List window (View → Panel List).
    pub(crate) fn draw_panel_list(&mut self, ctx: &eframe::egui::Context) {
        if !self.ui.show_panel_list { return; }
        let mut open = true;
        eframe::egui::Window::new("Panel List")
            .open(&mut open)
            .resizable(false)
            .default_width(250.0)
            .show(ctx, |ui| {
                for ps in &mut self.panel_layout.panels {
                    // Phantom panel with no draw site (see UiLayout::default) — hide its dead
                    // checkbox even when loaded from an old ui_layout.json.
                    if ps.id == PanelId::MaterialEditor { continue; }
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut ps.visible, ps.id.label());
                        ui.with_layout(eframe::egui::Layout::right_to_left(eframe::egui::Align::Center), |ui| {
                            let dock_label = match ps.dock {
                                DockZone::Left => "L",
                                DockZone::Right => "R",
                                DockZone::Bottom => "B",
                                DockZone::Floating => "F",
                            };
                            if ui.small_button(dock_label).on_hover_text("Cycle dock zone").clicked() {
                                ps.dock = match ps.dock {
                                    DockZone::Left => DockZone::Right,
                                    DockZone::Right => DockZone::Bottom,
                                    DockZone::Bottom => DockZone::Floating,
                                    DockZone::Floating => DockZone::Left,
                                };
                            }
                        });
                    });
                }
                ui.separator();
                if ui.button("Reset to Default Layout").clicked() {
                    self.panel_layout = UiLayout::default();
                    self.toast("Layout reset to default");
                }
                self.panel_layout.save_to_file();
            });
        if !open { self.ui.show_panel_list = false; }
    }
}
