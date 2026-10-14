//! Top and bottom viewport docks using the canonical panel-group system.

use crate::*;
use eframe::egui;

use super::*;

impl FourthLayerApp {
    pub(crate) fn ui2_horizontal_dock(
        &mut self,
        ctx: &egui::Context,
        side: DockSide,
    ) -> Option<Act> {
        debug_assert!(matches!(side, DockSide::Top | DockSide::Bottom));
        let docked: Vec<usize> = self
            .ui2
            .dock
            .iter()
            .enumerate()
            .filter_map(|(index, group)| (group.side == side).then_some(index))
            .collect();
        if docked.is_empty() {
            match side {
                DockSide::Top => self.ui2.top_panel_rect = egui::Rect::NOTHING,
                DockSide::Bottom => self.ui2.bottom_panel_rect = egui::Rect::NOTHING,
                _ => unreachable!(),
            }
            return None;
        }

        let available_height = ctx.available_rect().height().max(1.0);
        let stored_height: f32 = docked
            .iter()
            .map(|&index| self.ui2.dock[index].height.max(0.0))
            .fold(0.0, f32::max);
        let gap = 6.0;
        let preferred_height = if stored_height > 0.0 {
            stored_height
        } else {
            320.0
        }
        .clamp(160.0, (available_height * 0.75).max(160.0));

        let panel = match side {
            DockSide::Top => egui::TopBottomPanel::top("v2_dock_top_horizontal"),
            DockSide::Bottom => egui::TopBottomPanel::bottom("v2_dock_bottom_horizontal"),
            _ => unreachable!(),
        }
        .resizable(true)
        .default_height(preferred_height)
        .min_height(120.0)
        .max_height((available_height * 0.75).max(120.0))
        .show_separator_line(true)
        .frame(egui::Frame::none().fill(ui_theme::SURFACE_PANEL));

        let mut action = None;
        let mut tab_drag = self.ui2.tab_drag;
        let mut tab_float_drag = self.ui2.tab_float_drag;
        let response = panel.show(ctx, |ui| {
            let rect = ui.max_rect();
            ui.set_min_size(rect.size());
            ui.set_max_size(rect.size());
            ui.set_clip_rect(rect);

            let heights = fit_dock_heights(&self.ui2.dock, &docked, rect.height(), gap);
            let mut target_rects = Vec::with_capacity(docked.len());

            for (position, &group_index) in docked.iter().enumerate() {
                if position > 0 {
                    ui.add_space(gap);
                }
                let group = self.ui2.dock[group_index].clone();
                let card_height = heights[position].max(HEADER_H);
                let (card_rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), card_height),
                    egui::Sense::hover(),
                );
                target_rects.push((group.id, card_rect));

                ui.allocate_ui_at_rect(card_rect, |ui| {
                    ui.set_min_size(card_rect.size());
                    ui.set_max_size(card_rect.size());
                    ui.set_clip_rect(card_rect);
                    if !group.head_coll {
                        group_tab_strip(
                            ui,
                            group_index,
                            &group,
                            false,
                            &mut action,
                            &mut tab_drag,
                            &mut tab_float_drag,
                        );
                    } else {
                        let (_, restore) = ui.allocate_exact_size(
                            egui::vec2(card_rect.width(), 6.0),
                            egui::Sense::click(),
                        );
                        if restore.clicked() {
                            action = Some(Act::HeadCollapse(group_index));
                        }
                    }
                    if !group.collapsed && !group.head_coll {
                        let body_height = (card_height - HEADER_H).max(0.0);
                        let body_rect = egui::Rect::from_min_size(
                            ui.cursor().min,
                            egui::vec2(card_rect.width(), body_height),
                        );
                        let mut body_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(body_rect)
                                .layout(egui::Layout::top_down(egui::Align::Min)),
                        );
                        body_ui.set_min_size(body_rect.size());
                        body_ui.set_max_size(body_rect.size());
                        body_ui.set_clip_rect(body_rect);
                        fill_pane_body(self, &mut body_ui, &group);
                    }
                });
            }
            match side {
                DockSide::Top => self.ui2.top_rects = target_rects,
                DockSide::Bottom => self.ui2.bottom_rects = target_rects,
                _ => unreachable!(),
            }
        });

        let panel_rect = response.response.rect;
        match side {
            DockSide::Top => self.ui2.top_panel_rect = panel_rect,
            DockSide::Bottom => self.ui2.bottom_panel_rect = panel_rect,
            _ => unreachable!(),
        }
        if docked.len() == 1 {
            self.ui2.dock[docked[0]].height = panel_rect.height();
        }
        self.ui2.tab_drag = tab_drag;
        self.ui2.tab_float_drag = tab_float_drag;
        action
    }
}
