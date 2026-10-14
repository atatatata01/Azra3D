//! Geometry Channels panel — Modo-style procedural face/edge filters.
//!
//! Openable from Add Panel / View → Geometry Channels. **Not** in the default dock
//! layout; users open it when they need it.
//!
//! Body: edit a working channel, Select matching on the active poly mesh. Per-modifier
//! channels (Bevel, …) live on the modifier stack in Properties.

use eframe::egui;
use egui_phosphor::regular as ph;
use crate::*;
use core_math::geometry_channel::{channel_summary, GeoChannelKind, GeometryChannel};
use core_math::scene_graph::NodePayload;

impl AzraApp {
    /// Draw the Geometry Channels dock/floating panel body.
    pub(crate) fn ui2_draw_channels(&mut self, ui: &mut egui::Ui) {
        let h = ui.available_height();
        egui::ScrollArea::vertical()
            .id_salt("v2_channels_scroll")
            .auto_shrink([false, false])
            .max_height(h)
            .show(ui, |ui| {
                let iw = ui.available_width();
                ui.set_min_width(iw);
                ui.set_max_width(iw);
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.add_space(2.0);

                ui.label(
                    egui::RichText::new("Geometry Channels")
                        .size(13.0)
                        .strong()
                        .color(ui_theme::TEXT),
                );
                ui.label(
                    egui::RichText::new(
                        "Modo-style filters: which faces/edges a modifier affects, \
                         or select by topology / material / edge metrics.",
                    )
                    .size(10.0)
                    .color(ui_theme::TEXT_DIM),
                );
                ui.add_space(4.0);
                ui.separator();

                // ── Working channel (select-by-channel) ──────────────────────
                ui.label(
                    egui::RichText::new("SELECT BY CHANNEL")
                        .size(10.0)
                        .strong()
                        .color(ui_theme::TEXT_SECTION),
                );
                ui.label(
                    egui::RichText::new(
                        "Edit filter, then Select matching on the active poly body.",
                    )
                    .size(9.5)
                    .color(ui_theme::TEXT_DIM),
                );

                // Named polygroups of the active body, so the Polygroup kind's picker shows the
                // names the user authored instead of a bare id.
                let groups: Vec<(u16, String)> = self
                    .poly_active_body()
                    .and_then(|nid| self.poly_cd(nid))
                    .map(|cd| {
                        let mut g: Vec<(u16, String)> =
                            cd.group_meta.iter().map(|m| (m.id, m.name.clone())).collect();
                        g.sort_by_key(|(id, _)| *id);
                        g
                    })
                    .unwrap_or_default();
                let mut ch = self.ui2.geo_channel_work.clone();
                let _ = Self::draw_geo_channel_ui(ui, &mut ch, "geo_ch_panel_work", &groups);
                self.ui2.geo_channel_work = ch;

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(format!("{}  Select matching", ph::SELECTION_BACKGROUND))
                        .on_hover_text(
                            "Select faces or edges on the active poly that match this channel.\n\
                             Edge filters (length / angle) force Edge mode; others use Face mode.",
                        )
                        .clicked()
                    {
                        let work = self.ui2.geo_channel_work.clone();
                        self.poly_select_by_geo_channel(work);
                    }
                    if ui
                        .small_button("Reset")
                        .on_hover_text("Reset working channel to All geometry")
                        .clicked()
                    {
                        self.ui2.geo_channel_work = GeometryChannel::default();
                    }
                });

                ui.add_space(6.0);
                ui.separator();

                // ── Quick picks ──────────────────────────────────────────────
                ui.label(
                    egui::RichText::new("QUICK PICKS")
                        .size(10.0)
                        .strong()
                        .color(ui_theme::TEXT_SECTION),
                );
                ui.label(
                    egui::RichText::new("One-click load + select (uses active poly body).")
                        .size(9.5)
                        .color(ui_theme::TEXT_DIM),
                );

                let mut pending_select: Option<GeometryChannel> = None;
                ui.horizontal_wrapped(|ui| {
                    let picks: [(&str, GeoChannelKind); 7] = [
                        ("All faces", GeoChannelKind::All),
                        ("Triangles", GeoChannelKind::Tris),
                        ("Quads", GeoChannelKind::Quads),
                        ("N-gons", GeoChannelKind::Ngons),
                        ("5-sided", GeoChannelKind::NSides),
                        ("Material 0", GeoChannelKind::Material),
                        ("Edges ~90°", GeoChannelKind::EdgeAngle),
                    ];
                    for (lab, kind) in picks {
                        if ui.small_button(lab).clicked() {
                            let mut c = GeometryChannel::default();
                            c.kind = kind;
                            if kind == GeoChannelKind::NSides {
                                c.n_sides = 5;
                            }
                            if kind == GeoChannelKind::EdgeAngle {
                                c.angle_min_deg = 80.0;
                                c.angle_max_deg = 100.0;
                            }
                            self.ui2.geo_channel_work = c.clone();
                            pending_select = Some(c);
                        }
                    }
                });
                if let Some(c) = pending_select {
                    self.poly_select_by_geo_channel(c);
                }

                ui.add_space(8.0);
                ui.separator();

                // ── Modifier stack summary ───────────────────────────────────
                ui.label(
                    egui::RichText::new("MODIFIER CHANNELS")
                        .size(10.0)
                        .strong()
                        .color(ui_theme::TEXT_SECTION),
                );
                ui.label(
                    egui::RichText::new(
                        "Per-modifier geometry channels (Bevel, …) live on the modifier \
                         stack in Properties → Poly. Set the channel there so the \
                         modifier only affects matching faces/edges.",
                    )
                    .size(10.0)
                    .color(ui_theme::TEXT_DIM),
                );

                match self.sel.usel.active_node() {
                    Some(nid) => {
                        let summary: Option<Vec<(usize, &'static str, String)>> =
                            self.scene.scene_graph.get(nid).and_then(|n| {
                                if let NodePayload::PolygonalBody { modifiers, .. } = &n.payload {
                                    let rows: Vec<_> = modifiers
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, m)| !m.geo_channel.is_all())
                                        .map(|(i, m)| {
                                            (
                                                i,
                                                m.label(),
                                                channel_summary(&m.geo_channel),
                                            )
                                        })
                                        .collect();
                                    Some(rows)
                                } else {
                                    None
                                }
                            });
                        match summary {
                            None => {
                                ui.weak("Active object is not a poly mesh.");
                            }
                            Some(rows) if rows.is_empty() => {
                                ui.weak(
                                    "Active poly: no modifier has a filter (all = entire mesh).",
                                );
                            }
                            Some(rows) => {
                                ui.add_space(4.0);
                                for (i, ty, sum) in rows {
                                    ui.label(
                                        egui::RichText::new(format!("  [{i}] {ty}  →  {sum}"))
                                            .size(11.0)
                                            .color(ui_theme::TEXT),
                                    );
                                }
                            }
                        }
                    }
                    None => {
                        ui.weak("Select a poly body to see modifier channel filters.");
                    }
                }

                ui.add_space(8.0);
            });
    }
}
