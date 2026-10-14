// core_engine/app/src/ui_menus.rs
//
// ONE menu contract for the whole app. egui 0.32 closes a menu on ANY click inside it;
// AZRA's menus were written for the egui 0.29 contract: a menu stays open until an item
// calls `ui.close_menu()`, a click lands outside it, or Esc. That contract is what lets
// checkboxes, sliders, colour buttons and text fields live inside menus (Dither settings,
// modifier-wire colour/thickness, Rename Polygroup) — under the 0.32 default the first
// click on any of them closed the menu (and the colour picker with it).
//
// Every menu ROOT goes through here: `azra_menu_button` for menu buttons (sub-menus inherit
// the root's config through egui's UiStack tag) and `azra_context_menu` for right-click
// menus. Use these instead of `Ui::menu_button` / `Response::context_menu`.

use eframe::egui;
use egui::containers::menu::{is_in_menu, MenuButton, MenuConfig, SubMenuButton};

/// The app-wide menu config: close only on an explicit `close_menu()`, an outside click, or Esc.
fn azra_menu_config() -> MenuConfig {
    MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
}

pub(crate) trait AzraMenus {
    /// `Ui::menu_button` with the app menu contract. Inside a menu it is a sub-menu button,
    /// which inherits the root's config (so nested menus keep the same contract).
    fn azra_menu_button<'a, R>(
        &mut self,
        atoms: impl egui::IntoAtoms<'a>,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::InnerResponse<Option<R>>;
}

impl AzraMenus for egui::Ui {
    fn azra_menu_button<'a, R>(
        &mut self,
        atoms: impl egui::IntoAtoms<'a>,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::InnerResponse<Option<R>> {
        let (response, inner) = if is_in_menu(self) {
            SubMenuButton::new(atoms).ui(self, add_contents)
        } else {
            MenuButton::new(atoms).config(azra_menu_config()).ui(self, add_contents)
        };
        egui::InnerResponse::new(inner.map(|i| i.inner), response)
    }
}

pub(crate) trait AzraContextMenu {
    /// `Response::context_menu` with the app menu contract; the config is tagged on the
    /// popup's UiStack so sub-menus opened from it inherit the same contract.
    fn azra_context_menu(&self, add_contents: impl FnOnce(&mut egui::Ui)) -> Option<egui::InnerResponse<()>>;
}

impl AzraContextMenu for egui::Response {
    fn azra_context_menu(&self, add_contents: impl FnOnce(&mut egui::Ui)) -> Option<egui::InnerResponse<()>> {
        let config = azra_menu_config();
        egui::Popup::context_menu(self)
            .close_behavior(config.close_behavior)
            .info(egui::UiStackInfo::new(egui::UiKind::Menu).with_tag_value(MenuConfig::MENU_CONFIG_TAG, config))
            .show(add_contents)
    }
}

/// Right-click "Set as default / Reset default" for any toggle or setting. The default
/// persists to `~/.azra/user_defaults.json` under `key` (see `app_types::user_defaults`).
/// Chain it onto the widget's response: `ui.checkbox(..).default_menu("viewport.x", json!(v))`.
pub(crate) trait DefaultMenu: Sized {
    fn default_menu(self, key: &str, current: serde_json::Value) -> Self;
}

impl DefaultMenu for egui::Response {
    fn default_menu(self, key: &str, current: serde_json::Value) -> Self {
        let saved = app_types::user_defaults::get(key);
        let is_default = saved.as_ref() == Some(&current);
        self.azra_context_menu(|ui| {
            let set_label = if is_default { "✓ Is the default" } else { "Set as default" };
            if ui.add_enabled(!is_default, egui::Button::new(set_label)).clicked() {
                app_types::user_defaults::set(key, current.clone());
                ui.close();
            }
            if ui.add_enabled(saved.is_some(), egui::Button::new("Reset default")).clicked() {
                app_types::user_defaults::clear(key);
                ui.close();
            }
        });
        self
    }
}
