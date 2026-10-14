// core_engine/app/src/content_library.rs
// Content Library — folder scanning, text list, drag-drop, click-to-apply.

use eframe::egui;
use glam::Vec3;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use crate::{AzraApp, ContentCategory, ContentItem, LibraryDragPayload, ContentViewMode, DirtyFlags};
use crate::library_panel::folder_tree_to_disk;

/// The image extensions accepted as PBR texture MAPS (basecolor / roughness / normal / …).
/// `exr` + `tif`/`tiff` are here for the owner's downloaded PBR sets (float maps). All of these
/// decode through the `image` crate already in `app`'s deps (see `decode_to_map` /
/// `decode_file_to_map`): jpeg/png/exr are enabled Cargo features, tga/bmp/tiff are default-on
/// once requested. `exr` maps decode as linear float and are clamped to 8-bit on load — correct
/// for the [0,1] data channels PBR maps carry.
pub(crate) fn is_texture_map_ext(ext: &str) -> bool {
    matches!(
        ext,
        "jpg" | "jpeg" | "png" | "tga" | "bmp" | "exr" | "tif" | "tiff"
    )
}

// ==================================================================
// Background Thumbnail Loader (Phase 2+3: async decode + disk cache)
// ==================================================================
struct ThumbRequest {
    item_idx: usize,
    source_path: PathBuf,
    cache_path: Option<PathBuf>,
}
struct ThumbResult {
    item_idx: usize,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

pub(crate) struct ThumbnailBridge {
    tx: mpsc::Sender<ThumbRequest>,
    rx: mpsc::Receiver<ThumbResult>,
    /// Jobs accepted but not yet drained from `rx` (includes failed loads: worker drops them).
    /// Used so idle frames do not spin on `request_repaint`, but we keep polling while work flies.
    pending: std::sync::atomic::AtomicU32,
}

impl ThumbnailBridge {
    pub(crate) fn new() -> Self {
        let (req_tx, req_rx) = mpsc::channel::<ThumbRequest>();
        let (res_tx, res_rx) = mpsc::channel::<ThumbResult>();

        std::thread::Builder::new()
            .name("thumb-loader".into())
            .spawn(move || {
                while let Ok(req) = req_rx.recv() {
                    // Phase 3: check disk cache first
                    if let Some(ref cp) = req.cache_path {
                        if cp.exists() {
                            // Validate cache is newer than source
                            let cache_ok = match (
                                std::fs::metadata(cp).and_then(|m| m.modified()),
                                std::fs::metadata(&req.source_path).and_then(|m| m.modified()),
                            ) {
                                (Ok(cm), Ok(sm)) => sm <= cm,
                                _ => true, // if we can't check, trust cache
                            };
                            if cache_ok {
                                if let Ok(cached) = image::open(cp) {
                                    let rgba = cached.to_rgba8();
                                    let _ = res_tx.send(ThumbResult {
                                        item_idx: req.item_idx,
                                        rgba: rgba.clone().into_raw(),
                                        width: rgba.width(),
                                        height: rgba.height(),
                                    });
                                    continue;
                                }
                            }
                        }
                    }

                    // Full decode + downsample
                    let img = match image::open(&req.source_path) {
                        Ok(img) => img,
                        Err(_) => {
                            // Signal completion without a texture so pending can drain.
                            let _ = res_tx.send(ThumbResult {
                                item_idx: req.item_idx,
                                rgba: Vec::new(),
                                width: 0,
                                height: 0,
                            });
                            continue;
                        }
                    };
                    let thumb = img.thumbnail(128, 128).to_rgba8();
                    let width = thumb.width();
                    let height = thumb.height();
                    let rgba = thumb.into_raw();

                    // Phase 3: save to disk cache
                    if let Some(ref cp) = req.cache_path {
                        if let Some(parent) = cp.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = image::save_buffer(cp, &rgba, width, height, image::ColorType::Rgba8);
                    }

                    let _ = res_tx.send(ThumbResult {
                        item_idx: req.item_idx, rgba, width, height,
                    });
                }
            })
            .expect("spawn thumb-loader thread");

        Self {
            tx: req_tx,
            rx: res_rx,
            pending: std::sync::atomic::AtomicU32::new(0),
        }
    }

    fn request(&self, req: ThumbRequest) {
        self.pending
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.tx.send(req).is_err() {
            self.pending
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn poll(&self, max: usize) -> Vec<ThumbResult> {
        let mut results = Vec::new();
        for _ in 0..max {
            match self.rx.try_recv() {
                Ok(r) => {
                    self.pending
                        .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    results.push(r);
                }
                Err(_) => break,
            }
        }
        results
    }

    fn has_pending(&self) -> bool {
        self.pending.load(std::sync::atomic::Ordering::Relaxed) > 0
    }
}

/// FNV-1a 64-bit hash for cache file naming
fn fnv_hash(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Compute cache path for a source texture thumbnail
fn thumb_cache_path(source: &Path) -> PathBuf {
    let hash = fnv_hash(source.to_string_lossy().as_bytes());
    let dir = crate::KeyBindings::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("thumbcache");
    dir.join(format!("{:016x}.png", hash))
}

impl AzraApp {
    /// Owner #7: resolve a dropped library item (`item_idx` into `self.library.items`) to the
    /// texture file that should fill the target channel's map slot `mk`. A single-file texture /
    /// matcap item uses its only file; a PBR material SET picks the file whose detected role
    /// matches the channel (Albedo→basecolor, Roughness→roughness, …), falling back to the
    /// basecolor / first file so a diffuse drop always resolves.
    pub(crate) fn mp_resolve_drop_texture_path(
        &self,
        item_idx: usize,
        mk: core_math::pbr_material::MapKind,
    ) -> Option<std::path::PathBuf> {
        use core_math::pbr_material::MapKind;
        let item = self.library.items.get(item_idx)?;
        if item.files.is_empty() {
            return None;
        }
        // MapKind → the role string produced by `detect_texture_role`.
        let want_role: &str = match mk {
            MapKind::Albedo => "basecolor",
            MapKind::Normal => "normal",
            MapKind::Roughness => "roughness",
            MapKind::Metallic => "metallic",
            MapKind::Bump | MapKind::Displacement => "height",
            MapKind::Emission => "emission",
            // Library sets have no SSS-color role convention; nearest is the base color
            // (the fallbacks below still apply for single-file / role-less sets).
            MapKind::SssColor => "basecolor",
        };
        // Exact role match first.
        if let Some((_, p)) = item.files.iter().find(|(r, _)| r == want_role) {
            return Some(p.clone());
        }
        // Single-file item (loose texture / matcap): use it regardless of role name.
        if item.files.len() == 1 {
            return Some(item.files[0].1.clone());
        }
        // Multi-file set with no exact role: fall back to basecolor, then the first file.
        item.files
            .iter()
            .find(|(r, _)| r == "basecolor")
            .or_else(|| item.files.first())
            .map(|(_, p)| p.clone())
    }

    // ================================================================
    // Folder Scanning — MOVED to library_scan.rs (worker thread).
    // The synchronous scan (filesystem walk + full-res first-matcap decode +
    // upload_matcap_to_gpu FROM THE DRAW PATH) stalled every library open and
    // violated the Frame Ordering Contract. `request_library_scan()` spawns the
    // worker; `poll_library_scan()` installs items + does the GPU upload in poll.
    // ================================================================

    // ================================================================
    // Main UI — Bottom panel with left folder tree + right content
    // ================================================================
    pub(crate) fn draw_content_library(&mut self, ctx: &egui::Context) {
        if !self.library.open { return; }

        // Catalog from forever disk index or one async scan (not every open).
        self.ensure_library_scanned();

        // Ctrl+scroll zoom is handled inside the panel (see below)

        let mut apply_action: Option<(usize, ContentCategory)> = None;
        let mut close = false;
        let mut select_folder: Option<Option<String>> = None; // Some(None) = clear, Some(Some(x)) = select

        // Pre-compute filtered items
        let query_lower = self.library.search_query.to_lowercase();
        let active_cat = self.library.active_category;
        let sel_folder = self.library.selected_folder.clone();
        let filter_role = self.library.filter_role.clone();
        let filtered: Vec<usize> = self.library.items.iter()
            .enumerate()
            .filter(|(_, item)| item.category == active_cat)
            .filter(|(_, item)| query_lower.is_empty() || item.name.to_lowercase().contains(&query_lower))
            .filter(|(_, item)| sel_folder.is_none() || sel_folder.as_deref() == Some(&item.parent_folder))
            .filter(|(_, item)| filter_role.is_none() || filter_role.as_deref() == Some(&item.texture_role))
            .map(|(idx, _)| idx)
            .collect();

        // Enqueue thumbnail requests to background thread (max 8 per frame)
        let mut enqueued = 0u32;
        for &idx in &filtered {
            if enqueued >= 8 { break; }
            if self.library.items[idx].thumb_attempted { continue; }
            // Phase 1: prioritize existing thumb_path (tiny preview files)
            let source_and_cache = if let Some(ref tp) = self.library.items[idx].thumb_path {
                Some((tp.clone(), None)) // already a thumb, no cache needed
            } else {
                let img_path = if self.library.items[idx].is_individual_file {
                    self.library.items[idx].files.first().map(|(_, p)| p.clone())
                } else {
                    self.library.items[idx].files.iter().find(|(r, _)| r == "basecolor")
                        .or_else(|| self.library.items[idx].files.first())
                        .map(|(_, p)| p.clone())
                };
                img_path.map(|p| {
                    let cache = thumb_cache_path(&p);
                    (p, Some(cache))
                })
            };
            self.library.items[idx].thumb_attempted = true;
            if let Some((path, cache_path)) = source_and_cache {
                self.thumb_bridge.request(ThumbRequest {
                    item_idx: idx, source_path: path, cache_path,
                });
                enqueued += 1;
            }
        }

        // Poll completed thumbnails from background thread (max 6 per frame)
        {
            let results = self.thumb_bridge.poll(6);
            let mut any_loaded = false;
            for res in results {
                if res.width == 0 || res.rgba.is_empty() {
                    continue;
                }
                if res.item_idx < self.library.items.len() {
                    let size = [res.width as usize, res.height as usize];
                    let ci = egui::ColorImage::from_rgba_unmultiplied(size, &res.rgba);
                    let handle = ctx.load_texture(
                        format!("lib_thumb_{}", res.item_idx), ci, egui::TextureOptions::LINEAR,
                    );
                    self.library.items[res.item_idx].thumb_handle = Some(handle);
                    any_loaded = true;
                }
            }
            // B3d: UI-only — delayed repaint, never mark_dirty / request_repaint().
            if any_loaded || enqueued > 0 || self.thumb_bridge.has_pending() {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }

        let panel_resp = egui::TopBottomPanel::bottom("content_library")
            .exact_height(self.library.height)
            .show(ctx, |ui| {
                // ---- Drag grip at top edge ----
                let grip_resp = ui.allocate_response(
                    egui::vec2(ui.available_width(), 6.0),
                    egui::Sense::drag(),
                );
                if grip_resp.hovered() || grip_resp.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }
                if grip_resp.dragged() {
                    self.library.height = (self.library.height - grip_resp.drag_delta().y).clamp(100.0, 600.0);
                }
                // Draw grip visual: line + three dots
                let r = grip_resp.rect;
                ui.painter().rect_filled(r, 0.0, egui::Color32::from_gray(45));
                let cy = r.center().y;
                let cx = r.center().x;
                for dx in [-10.0_f32, 0.0, 10.0] {
                    ui.painter().circle_filled(egui::pos2(cx + dx, cy), 1.5, egui::Color32::from_gray(100));
                }

                // ---- Ctrl+Scroll zoom ----
                // Consume scroll wheel events when Ctrl is held to prevent ScrollArea from scrolling
                let ctrl_held = ui.input(|i| i.modifiers.command);
                if ctrl_held {
                    let scroll_y: f32 = ui.input_mut(|i| {
                        let mut total = 0.0_f32;
                        i.events.retain(|e| {
                            if let egui::Event::MouseWheel { delta, .. } = e {
                                total += delta.y;
                                false // consume the event
                            } else {
                                true
                            }
                        });
                        total
                    });
                    if scroll_y.abs() > 0.0 {
                        self.library.thumb_scale = (self.library.thumb_scale + scroll_y * 0.1).clamp(0.5, 3.0);
                    }
                }

                // ---- Header row ----
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Library").strong());
                    ui.separator();

                    for &cat in ContentCategory::all() {
                        let selected = self.library.active_category == cat;
                        if ui.selectable_label(selected, format!("{} {}", cat.icon(), cat.label())).clicked() {
                            self.library.active_category = cat;
                            self.library.selected_folder = None;
                            self.library.filter_role = None;
                        }
                    }

                    ui.separator();
                    ui.add(egui::TextEdit::singleline(&mut self.library.search_query)
                        .desired_width(100.0).hint_text("Filter..."));

                    ui.separator();
                    // Grid / List toggle
                    if ui.selectable_label(self.library.content_view == ContentViewMode::Grid, "Grid").clicked() {
                        self.library.content_view = ContentViewMode::Grid;
                    }
                    if ui.selectable_label(self.library.content_view == ContentViewMode::List, "List").clicked() {
                        self.library.content_view = ContentViewMode::List;
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("\u{2716}").clicked() { close = true; }
                        ui.weak(format!("{} items  {:.0}%", filtered.len(), self.library.thumb_scale * 100.0));
                    });
                });

                ui.separator();

                // ---- Three-pane layout using nested panels (finite width fix) ----

                // ==== LEFT PANE: Folder tree ====
                egui::SidePanel::left("lib_folder_tree")
                    .resizable(true)
                    .default_width(160.0)
                    .width_range(80.0..=300.0)
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical().id_salt("lib_tree").auto_shrink([false, false]).show(ui, |ui| {
                            // "All" entry
                            let all_selected = self.library.selected_folder.is_none();
                            if ui.selectable_label(all_selected, "All").clicked() {
                                select_folder = Some(None);
                            }

                            // Collect unique parent_folder values for active category
                            let mut folders: Vec<String> = self.library.items.iter()
                                .filter(|i| i.category == active_cat)
                                .filter(|i| query_lower.is_empty() || i.name.to_lowercase().contains(&query_lower))
                                .map(|i| i.parent_folder.clone())
                                .filter(|f| !f.is_empty())
                                .collect::<std::collections::HashSet<_>>()
                                .into_iter().collect();
                            folders.sort();

                            for folder in &folders {
                                let is_sel = self.library.selected_folder.as_deref() == Some(folder);
                                let count = self.library.items.iter()
                                    .filter(|i| i.category == active_cat && i.parent_folder == *folder)
                                    .count();
                                let expanded = self.library.expanded_folders.contains(folder);
                                ui.horizontal(|ui| {
                                    let arrow = if expanded { "\u{25BC}" } else { "\u{25B6}" };
                                    if ui.small_button(arrow).clicked() {
                                        if expanded { self.library.expanded_folders.remove(folder); }
                                        else { self.library.expanded_folders.insert(folder.clone()); }
                                    }
                                    let label = format!("\u{1F4C1} {} ({})", folder, count);
                                    if ui.selectable_label(is_sel, &label).clicked() {
                                        select_folder = Some(Some(folder.clone()));
                                    }
                                });
                                if expanded {
                                    let sub_items: Vec<String> = self.library.items.iter()
                                        .filter(|i| i.category == active_cat && i.parent_folder == *folder)
                                        .map(|i| i.name.clone())
                                        .collect();
                                    for sub in &sub_items {
                                        ui.horizontal(|ui| {
                                            ui.add_space(20.0);
                                            ui.label(egui::RichText::new(sub).size(10.0).weak());
                                        });
                                    }
                                }
                            }
                        });
                    });

                // ==== MIDDLE PANE: Filters ====
                egui::SidePanel::left("lib_filters_panel")
                    .resizable(true)
                    .default_width(120.0)
                    .width_range(60.0..=200.0)
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical().id_salt("lib_filters").auto_shrink([false, false]).show(ui, |ui| {
                            ui.label(egui::RichText::new("Filters").strong().size(11.0));
                            ui.separator();
                            let all_sel = self.library.filter_role.is_none();
                            if ui.selectable_label(all_sel, "All Types").clicked() {
                                self.library.filter_role = None;
                            }
                            let mut roles: Vec<String> = self.library.items.iter()
                                .filter(|i| i.category == active_cat)
                                .filter(|i| sel_folder.is_none() || sel_folder.as_deref() == Some(&i.parent_folder))
                                .filter(|i| !i.texture_role.is_empty())
                                .map(|i| i.texture_role.clone())
                                .collect::<std::collections::HashSet<_>>()
                                .into_iter().collect();
                            roles.sort();
                            for role in &roles {
                                let count = self.library.items.iter()
                                    .filter(|i| i.category == active_cat && i.texture_role == *role)
                                    .filter(|i| sel_folder.is_none() || sel_folder.as_deref() == Some(&i.parent_folder))
                                    .count();
                                let is_sel = self.library.filter_role.as_deref() == Some(role);
                                if ui.selectable_label(is_sel, format!("{} ({})", role, count)).clicked() {
                                    self.library.filter_role = Some(role.clone());
                                }
                            }
                        });
                    });

                // ==== CENTRAL PANE: Content grid (finite width from nested panels) ====
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("lib_content").auto_shrink([false, false]).show(ui, |ui| {
                        if filtered.is_empty() {
                            let msg = if self.library_scan_rx.is_some() { "Scanning library…" } else { "No items found" };
                            ui.centered_and_justified(|ui| { ui.weak(msg); });
                        } else if self.library.content_view == ContentViewMode::Grid {
                            let thumb_sz = (80.0 * self.library.thumb_scale).max(40.0);
                            let item_w = thumb_sz;
                            let item_h = thumb_sz + 28.0;
                            let spacing = 12.0;

                            // Calculate exact column count from available width
                            let avail_w = ui.available_width();
                            let cols = ((avail_w + spacing) / (item_w + spacing)).floor().max(1.0) as usize;

                            for row in filtered.chunks(cols) {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = spacing;
                                    for &item_idx in row {
                                        let item = &self.library.items[item_idx];
                                        ui.allocate_ui(egui::vec2(item_w, item_h), |ui| {
                                            ui.vertical_centered(|ui| {
                                                if let Some(ref handle) = item.thumb_handle {
                                                    let sized = egui::load::SizedTexture::new(handle.id(), egui::vec2(thumb_sz, thumb_sz));
                                                    let resp = ui.add(egui::Image::new(sized).sense(egui::Sense::click_and_drag()));
                                                    if resp.clicked() { apply_action = Some((item_idx, item.category)); }
                                                    if resp.dragged() && resp.drag_delta().length() > 4.0 {
                                                        egui::DragAndDrop::set_payload(ui.ctx(), LibraryDragPayload { item_idx, category: item.category });
                                                    }
                                                    resp.on_hover_text(&item.name);
                                                } else {
                                                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(thumb_sz, thumb_sz), egui::Sense::click_and_drag());
                                                    let bg_b = if item.thumb_attempted {
                                                        let t = (ui.input(|i| i.time) * 2.0).sin() as f32;
                                                        ((t * 0.5 + 0.5) * 30.0 + 45.0) as u8
                                                    } else { 65 };
                                                    ui.painter().rect_filled(rect, 4.0, egui::Color32::from_rgb(50, 55, bg_b));
                                                    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER,
                                                        item.category.icon(), egui::FontId::proportional(20.0 * self.library.thumb_scale),
                                                        egui::Color32::from_gray(120));
                                                    if resp.clicked() { apply_action = Some((item_idx, item.category)); }
                                                    if resp.dragged() && resp.drag_delta().length() > 4.0 {
                                                        egui::DragAndDrop::set_payload(ui.ctx(), LibraryDragPayload { item_idx, category: item.category });
                                                    }
                                                    resp.on_hover_text(&item.name);
                                                }
                                                ui.add_space(2.0);
                                                ui.add(egui::Label::new(
                                                    egui::RichText::new(&item.name)
                                                        .size((11.0 * self.library.thumb_scale).min(16.0))
                                                        .color(egui::Color32::from_gray(200))
                                                ).truncate());
                                            });
                                        });
                                    }
                                });
                                ui.add_space(spacing);
                            }
                        } else {
                            // List mode: single column with role tags
                            for &item_idx in &filtered {
                                let item = &self.library.items[item_idx];
                                let role_tag = if item.is_individual_file && !item.texture_role.is_empty() {
                                    format!("[{}] ", &item.texture_role)
                                } else { String::new() };
                                let label = format!("{} {}{}", item.category.icon(), role_tag, item.name);
                                let resp = ui.add(egui::Label::new(&label).sense(egui::Sense::click_and_drag()));
                                if resp.clicked() {
                                    apply_action = Some((item_idx, item.category));
                                }
                                if resp.dragged() && resp.drag_delta().length() > 4.0 {
                                    egui::DragAndDrop::set_payload(ui.ctx(), LibraryDragPayload {
                                        item_idx, category: item.category,
                                    });
                                }
                                resp.on_hover_text(&item.name);
                            }
                        }
                    });
                });
            });

        let _ = panel_resp; // used for layout

        // ---- Apply deferred folder selection ----
        if let Some(folder) = select_folder {
            self.library.selected_folder = folder;
        }

        if close {
            self.library.open = false;
            self.library.scanned = false; // rescan on next open to pick up new files
        }

        // ---- Handle click apply (deferred) ----
        if let Some((idx, cat)) = apply_action {
            match cat {
                ContentCategory::Materials | ContentCategory::Textures => {
                    self.apply_library_material_to_selection(idx);
                }
                ContentCategory::HDRIs => {
                    self.apply_library_hdri(idx);
                }
                ContentCategory::MatCaps => {
                    self.apply_library_matcap(idx);
                }
                ContentCategory::Bodies => {
                    let _ = self.insert_library_body_item(idx, None);
                }
            }
        }
    }

    // ================================================================
    // Handle Library Drag-Drop onto Viewport
    // ================================================================
    pub(crate) fn handle_library_drag_drop(&mut self, ctx: &egui::Context) {
        // Check for active library drag payload
        let payload = egui::DragAndDrop::payload::<LibraryDragPayload>(ctx);
        if payload.is_none() { return; }

        // Draw drag ghost near cursor
        if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
            if let Some(ref pl) = payload {
                if pl.item_idx < self.library.items.len() {
                    let name = self.library.items[pl.item_idx].name.clone();
                    let ghost_pos = pos + egui::vec2(12.0, 12.0);
                    egui::Area::new(egui::Id::new("lib_drag_ghost"))
                        .fixed_pos(ghost_pos)
                        .order(egui::Order::Tooltip)
                        .interactable(false)
                        .show(ctx, |ui| {
                            egui::Frame::none()
                                .fill(egui::Color32::from_rgba_unmultiplied(30, 40, 55, 220))
                                .rounding(4.0)
                                .inner_margin(egui::Margin::same(4))
                                .show(ui, |ui| {
                                    ui.label(egui::RichText::new(&name).size(11.0).color(egui::Color32::from_gray(200)));
                                });
                        });
                }
            }
        }

        // Drop on release
        if ctx.input(|i| i.pointer.any_released()) {
            if let Some(pl) = payload {
                let pl = pl.as_ref().clone();
                // Folder PBR materials take the graph-material import path (material_import.rs):
                // creates + assigns a scene material with Color/Normal/Roughness maps, box-projected.
                if self.drag_is_folder_material(&pl) {
                    self.drop_library_material_on_viewport(pl.item_idx, ctx);
                    return;
                }
                match pl.category {
                    ContentCategory::Materials | ContentCategory::Textures => {
                        // hovered_prim is cleared while dragging from the library (viewport
                        // not hovered) — raycast at the release position instead.
                        let pos = ctx.input(|i| i.pointer.latest_pos().or(i.pointer.hover_pos()));
                        let under = pos.and_then(|p| self.prim_under_screen(p));
                        if let Some(prim_idx) = under.or(self.active_prim_index_unified()) {
                            self.apply_library_material(pl.item_idx, prim_idx);
                        } else {
                            self.toast("Drop on a primitive to apply material");
                        }
                    }
                    ContentCategory::HDRIs => {
                        self.apply_library_hdri(pl.item_idx);
                    }
                    ContentCategory::MatCaps => {
                        self.apply_library_matcap(pl.item_idx);
                    }
                    ContentCategory::Bodies => {
                        let pos = ctx.input(|i| i.pointer.latest_pos().or(i.pointer.hover_pos()));
                        let world = pos.map(|p| self.ui2_world_under_cursor(p));
                        let _ = self.insert_library_body_item(pl.item_idx, world);
                    }
                }
            }
        }
    }

    /// Insert a kitbash `.flb` library tile into the scene (Bodies category).
    /// Starts interactive place (Ctrl surface / scroll resize) on success.
    pub(crate) fn insert_library_body_item(
        &mut self,
        item_idx: usize,
        world_pos: Option<glam::Vec3>,
    ) -> Option<core_math::scene_graph::NodeId> {
        let Some(item) = self.library.items.get(item_idx) else {
            self.toast("Library item missing");
            return None;
        };
        if item.category != ContentCategory::Bodies {
            return None;
        }
        let path = item
            .files
            .iter()
            .find(|(r, _)| r == "body")
            .map(|(_, p)| p.clone())
            .or_else(|| item.files.first().map(|(_, p)| p.clone()));
        let Some(path) = path else {
            self.toast("Body library file missing");
            return None;
        };
        self.insert_body_from_library_at(&path, world_pos)
    }

    // ================================================================
    // Apply Material
    // ================================================================
    pub(crate) fn apply_library_material(&mut self, item_idx: usize, prim_idx: usize) {
        if prim_idx >= self.scene.primitives.len() || item_idx >= self.library.items.len() { return; }

        let item_name = self.library.items[item_idx].name.clone();
        let is_individual = self.library.items[item_idx].is_individual_file;

        // For individual texture files, use the file path directly
        let basecolor_path = if is_individual {
            self.library.items[item_idx].files.first().map(|(_, p)| p.clone())
        } else {
            self.library.items[item_idx].files.iter()
                .find(|(role, _)| role == "basecolor")
                .map(|(_, p)| p.clone())
        };

        if let Some(ref path) = basecolor_path {
            if let Ok(img) = image::open(path) {
                let small = img.thumbnail(16, 16).to_rgba8();
                let (mut r_sum, mut g_sum, mut b_sum) = (0u64, 0u64, 0u64);
                let count = (small.width() * small.height()) as u64;
                if count > 0 {
                    for pixel in small.pixels() {
                        r_sum += pixel[0] as u64;
                        g_sum += pixel[1] as u64;
                        b_sum += pixel[2] as u64;
                    }
                    let albedo = Vec3::new(
                        r_sum as f32 / (count as f32 * 255.0),
                        g_sum as f32 / (count as f32 * 255.0),
                        b_sum as f32 / (count as f32 * 255.0),
                    );

                    self.transaction("Apply Library Material", |s| {
                        // For individual files, use role detection
                        if is_individual {
                            let role = detect_texture_role(&item_name);
                            match role {
                                "roughness" => {
                                    let lum = albedo.x * 0.299 + albedo.y * 0.587 + albedo.z * 0.114;
                                    s.write_prim(prim_idx, |p| p.material.roughness = lum);
                                }
                                "metallic" => {
                                    let lum = albedo.x * 0.299 + albedo.y * 0.587 + albedo.z * 0.114;
                                    s.write_prim(prim_idx, |p| p.material.metallic = lum);
                                }
                                _ => {
                                    // Load into GPU atlas for basecolor
                                    let loaded_tex_id = s.atlas_add_texture(path);
                                    s.write_prim(prim_idx, |p| {
                                        p.material.albedo = albedo;
                                        if let Some(tex_id) = loaded_tex_id {
                                            p.texture_id = tex_id;
                                        }
                                    });
                                }
                            }
                        } else {
                            // Load basecolor into GPU atlas
                            let loaded_tex_id = s.atlas_add_texture(path);
                            // Heuristic roughness/metallic from folder name
                            let name_lower = item_name.to_lowercase();
                            let (met, rough) = if name_lower.contains("metal") || name_lower.contains("steel")
                                || name_lower.contains("chrome") || name_lower.contains("silver")
                                || name_lower.contains("gold")
                            {
                                (0.85, 0.25)
                            } else if name_lower.contains("marble") || name_lower.contains("tile")
                                || name_lower.contains("glass")
                            {
                                (0.0, 0.15)
                            } else if name_lower.contains("wood") || name_lower.contains("fabric")
                                || name_lower.contains("rug") || name_lower.contains("weave")
                            {
                                (0.0, 0.7)
                            } else if name_lower.contains("concrete") || name_lower.contains("wall")
                                || name_lower.contains("plaster") || name_lower.contains("dirt")
                                || name_lower.contains("gravel") || name_lower.contains("mud")
                            {
                                (0.0, 0.8)
                            } else if name_lower.contains("rust") || name_lower.contains("paint") {
                                (0.3, 0.6)
                            } else {
                                (0.0, 0.5)
                            };
                            s.write_prim(prim_idx, |p| {
                                p.material.albedo = albedo;
                                if let Some(tex_id) = loaded_tex_id {
                                    p.texture_id = tex_id;
                                }
                                p.material.metallic = met;
                                p.material.roughness = rough;
                            });
                        }

                        s.mark_dirty(DirtyFlags::RENDER);
                        s.toast(&format!("Applied: {}", item_name));
                    });
                    return;
                }
            }
        }

        // Fallback
        self.transaction("Apply Library Material", |s| {
            s.write_prim(prim_idx, |p| {
                p.material.roughness = 0.5;
                p.material.metallic = 0.0;
            });
            s.mark_dirty(DirtyFlags::RENDER);
            s.toast(&format!("Applied: {} (no image found)", item_name));
        });
    }

    // ================================================================
    // Apply HDRI (real loading via parse_hdr + upload_env_to_gpu)
    // ================================================================
    pub(crate) fn apply_library_hdri(&mut self, item_idx: usize) {
        if item_idx >= self.library.items.len() { return; }
        let item_name = self.library.items[item_idx].name.clone();
        let env_file = self.library.items[item_idx].files.iter()
            .find(|(role, _)| role == "env" || role == "ref")
            .map(|(_, path)| path.clone());

        if let Some(hdr_path) = env_file {
            // Worker-thread decode + poll install (env_ibl.rs) — the click never stalls
            // the frame, and the split-sum IBL chain is rebuilt for the new HDRI.
            self.render.exposure = 1.0; // reset exposure for HDR input
            self.request_env_load(std::path::PathBuf::from(hdr_path));
            self.toast(&format!("Loading HDR: {}…", item_name));
        } else {
            self.toast(&format!("No .hdr file found in {}", item_name));
        }
    }

    // ================================================================
    // Apply MatCap
    // ================================================================
    pub(crate) fn apply_library_matcap(&mut self, item_idx: usize) {
        if item_idx >= self.library.items.len() { return; }
        let item_name = self.library.items[item_idx].name.clone();
        let mc_file = self.library.items[item_idx].files.iter()
            .find(|(role, _)| role == "matcap")
            .map(|(_, path)| path.clone());

        if let Some(path) = mc_file {
            if self.load_matcap_from_path(&path, &item_name) {
                // Auto-switch to MatCap shading mode
                self.render.shading_mode = crate::ShadingMode::MatCap;
                self.render.dither_enabled = false; // MatCap is clean — no dither
                self.mark_dirty(DirtyFlags::RENDER);
                self.toast(&format!("MatCap: {} ({}x{})", item_name, self.matcap_width, self.matcap_height));
                return;
            }
            self.toast(&format!("Failed to load matcap: {}", item_name));
        } else {
            self.toast(&format!("No image file found in {}", item_name));
        }
    }

    /// Load RGBA matcap pixels from disk and upload to GPU. Returns true on success.
    pub(crate) fn load_matcap_from_path(&mut self, path: &std::path::Path, label: &str) -> bool {
        match image::open(path) {
            Ok(img) => {
                let img = img.to_rgba8();
                self.matcap_width = img.width();
                self.matcap_height = img.height();
                self.matcap_data = img.into_raw();
                self.upload_matcap_to_gpu();
                let _ = label;
                true
            }
            Err(_) => false,
        }
    }

    /// `%LOCALAPPDATA%/Azra/default_matcap.txt` — absolute path or stem name of the preferred matcap.
    pub(crate) fn default_matcap_prefs_path() -> std::path::PathBuf {
        let dir = std::env::var("LOCALAPPDATA")
            .ok()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                    .unwrap_or_else(std::env::temp_dir)
            })
            .join("Azra");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("default_matcap.txt")
    }

    pub(crate) fn load_default_matcap_pref() -> Option<String> {
        let p = Self::default_matcap_prefs_path();
        let s = std::fs::read_to_string(p).ok()?;
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    }

    pub(crate) fn save_default_matcap_pref(path_or_name: &str) -> bool {
        let p = Self::default_matcap_prefs_path();
        std::fs::write(p, path_or_name.trim()).is_ok()
    }

    /// Context menu: set this library matcap as the default, apply it now, and switch to MatCap mode.
    pub(crate) fn set_default_matcap_from_library(&mut self, item_idx: usize) {
        if item_idx >= self.library.items.len() {
            return;
        }
        let item_name = self.library.items[item_idx].name.clone();
        let mc_path = self.library.items[item_idx]
            .files
            .iter()
            .find(|(role, _)| role == "matcap")
            .map(|(_, path)| path.clone());
        let Some(path) = mc_path else {
            self.toast(&format!("No image file found in {}", item_name));
            return;
        };
        // Prefer absolute path so renames of the asset root still resolve when possible.
        let store = path
            .canonicalize()
            .unwrap_or(path.clone())
            .to_string_lossy()
            .to_string();
        if !Self::save_default_matcap_pref(&store) {
            self.toast("Could not save default matcap preference");
            return;
        }
        if self.load_matcap_from_path(&path, &item_name) {
            self.render.shading_mode = crate::ShadingMode::MatCap;
            self.render.dither_enabled = false;
            self.mark_dirty(DirtyFlags::RENDER);
            self.toast(&format!("Default matcap: {item_name}"));
        } else {
            self.toast(&format!("Saved default, but failed to load: {item_name}"));
        }
    }

    // ================================================================
    // Upload MatCap texture to GPU (storage buffer, binding 42)
    // ================================================================
    pub(crate) fn upload_matcap_to_gpu(&mut self) {
        if let Some(ref mut gpu) = self.gpu {
            let device = gpu.device.clone();
            let queue = gpu.queue.clone();
            {
                // Pack RGBA8 pixels as u32 array: [width, height, pixel0, pixel1, ...]
                let pixel_count = (self.matcap_width * self.matcap_height) as usize;
                let mut buf = Vec::with_capacity(2 + pixel_count);
                buf.push(self.matcap_width);
                buf.push(self.matcap_height);
                for i in 0..pixel_count {
                    let off = i * 4;
                    if off + 3 < self.matcap_data.len() {
                        let r = self.matcap_data[off] as u32;
                        let g = self.matcap_data[off + 1] as u32;
                        let b = self.matcap_data[off + 2] as u32;
                        let a = self.matcap_data[off + 3] as u32;
                        buf.push((a << 24) | (b << 16) | (g << 8) | r);
                    }
                }
                let data_bytes: &[u8] = bytemuck::cast_slice(&buf);
                gpu.buffers.matcap_buffer = device.create_buffer(&eframe::wgpu::BufferDescriptor {
                    label: Some("MatCapData"), size: data_bytes.len() as u64,
                    usage: eframe::wgpu::BufferUsages::STORAGE | eframe::wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                queue.write_buffer(&gpu.buffers.matcap_buffer, 0, data_bytes);
                // Invalidate deferred bind group (new buffer)
                gpu.bind_groups.deferred_bg = None;
            }
        }
    }

    // ================================================================
    // Apply Dropped Image File (OS drag-drop onto viewport)
    // ================================================================
    pub(crate) fn apply_dropped_image(&mut self, path: &Path, prim_idx: usize) {
        if prim_idx >= self.scene.primitives.len() { return; }
        let fname = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let role = detect_texture_role(&fname);

        if let Ok(img) = image::open(path) {
            let small = img.thumbnail(16, 16).to_rgba8();
            let (mut r_sum, mut g_sum, mut b_sum) = (0u64, 0u64, 0u64);
            let count = (small.width() * small.height()) as u64;
            if count > 0 {
                for pixel in small.pixels() {
                    r_sum += pixel[0] as u64;
                    g_sum += pixel[1] as u64;
                    b_sum += pixel[2] as u64;
                }
                let avg = Vec3::new(
                    r_sum as f32 / (count as f32 * 255.0),
                    g_sum as f32 / (count as f32 * 255.0),
                    b_sum as f32 / (count as f32 * 255.0),
                );

                self.transaction("Apply Dropped Texture", |s| {
                    match role {
                        "basecolor" => {
                            let loaded_tex_id = s.atlas_add_texture(path);
                            s.write_prim(prim_idx, |p| {
                                p.material.albedo = avg;
                                if let Some(tex_id) = loaded_tex_id {
                                    p.texture_id = tex_id;
                                }
                            });
                        }
                        "roughness" => {
                            let lum = avg.x * 0.299 + avg.y * 0.587 + avg.z * 0.114;
                            s.write_prim(prim_idx, |p| p.material.roughness = lum);
                        }
                        "metallic" => {
                            let lum = avg.x * 0.299 + avg.y * 0.587 + avg.z * 0.114;
                            s.write_prim(prim_idx, |p| p.material.metallic = lum);
                        }
                        _ => {
                            let loaded_tex_id = s.atlas_add_texture(path);
                            s.write_prim(prim_idx, |p| {
                                p.material.albedo = avg;
                                if let Some(tex_id) = loaded_tex_id {
                                    p.texture_id = tex_id;
                                }
                            });
                        }
                    }
                    s.mark_dirty(DirtyFlags::RENDER);
                    s.toast(&format!("Applied {} from: {}", role, fname));
                });
            }
        } else {
            self.toast(&format!("Failed to load image: {}", fname));
        }
    }

    // ================================================================
    // Startup MatCap Preload — runs once on first frame, no full scan
    // ================================================================
    pub(crate) fn preload_startup_matcap(&mut self) {
        if self.library.matcap_preloaded { return; }
        self.library.matcap_preloaded = true;
        if !self.matcap_data.is_empty() { return; }

        // 1) User-chosen default from matcap context menu (absolute path or stem).
        if let Some(pref) = Self::load_default_matcap_pref() {
            let pref_path = std::path::PathBuf::from(&pref);
            if pref_path.is_file() {
                if self.load_matcap_from_path(&pref_path, &pref) {
                    let name = pref_path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy();
                    eprintln!(
                        "[Azra] Startup matcap (user default): {} ({}x{})",
                        name, self.matcap_width, self.matcap_height
                    );
                    return;
                }
            }
            // Fall through: resolve by stem name under Matcaps/
            if let Some(root) = find_asset_root() {
                let matcap_dir = root.join("Matcaps");
                if matcap_dir.is_dir() {
                    if let Ok(entries) = std::fs::read_dir(&matcap_dir) {
                        let stem = pref_path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or(pref.as_str());
                        for e in entries.flatten() {
                            let p = e.path();
                            if !p.is_file() {
                                continue;
                            }
                            let match_stem = p
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .map_or(false, |s| s.eq_ignore_ascii_case(stem));
                            if match_stem {
                                if self.load_matcap_from_path(&p, stem) {
                                    eprintln!(
                                        "[Azra] Startup matcap (user default name): {} ({}x{})",
                                        stem, self.matcap_width, self.matcap_height
                                    );
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        }

        let Some(root) = find_asset_root() else { return; };
        let matcap_dir = root.join("Matcaps");
        if !matcap_dir.is_dir() { return; }

        let Ok(entries) = std::fs::read_dir(&matcap_dir) else { return; };
        let mut files: Vec<PathBuf> = entries.flatten()
            .filter_map(|e| {
                let p = e.path();
                if !p.is_file() { return None; }
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "exr") { Some(p) } else { None }
            })
            .collect();
        if files.is_empty() { return; }
        files.sort();

        // Prefer "3pointBrown" by name, otherwise take the first file
        let chosen = files.iter()
            .find(|p| {
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .map_or(false, |s| s.eq_ignore_ascii_case("3pointBrown"))
            })
            .unwrap_or(&files[0]);

        if self.load_matcap_from_path(chosen, &chosen.file_stem().unwrap_or_default().to_string_lossy()) {
            let name = chosen.file_stem().unwrap_or_default().to_string_lossy();
            eprintln!("[Azra] Startup matcap preloaded: {} ({}x{})", name, self.matcap_width, self.matcap_height);
        }
    }

    /// Ensure the library catalog is available (idempotent until `scanned` is cleared).
    ///
    /// 1. If already scanned → no-op.
    /// 2. If a scan worker is already flying → wait for poll.
    /// 3. **Cold start only** (`items` empty): load forever disk index
    ///    (`%LOCALAPPDATA%\Azra\library_index.json`) — instant, no assets/ walk.
    /// 4. Else (rename/delete/add cleared `scanned` with items still present) → full disk scan;
    ///    result overwrites the forever index.
    ///
    /// Force wipe: `force_library_rescan()` or env `AZRA_LIB_RESCAN=1`.
    pub(crate) fn ensure_library_scanned(&mut self) {
        if self.library.scanned {
            return;
        }
        if self.library_scan_rx.is_some() {
            return; // worker in flight — poll_library_scan will install
        }
        let force_env = std::env::var_os("AZRA_LIB_RESCAN").is_some();
        if force_env {
            crate::library_index_cache::invalidate_library_index();
            self.library.items.clear();
        }
        // Forever cache is only for cold start. After an in-session edit that cleared
        // `scanned`, items are still populated — must re-walk disk, not reinstall stale cache.
        if self.library.items.is_empty() && !force_env {
            if let Some(cached) = crate::library_index_cache::load_library_index() {
                if cached.found_root || !cached.items.is_empty() {
                    self.library.items = cached.items;
                    self.library.next_item_id = cached.next_item_id;
                    self.library.asset_root_display = cached.asset_root_display;
                    self.library.scanned = true;
                    self.ui2.library.invalidate_content_caches();
                    self.ui2.library.extra_folders = crate::library_folders::load();
                    // Seed default HDR only when nothing is loaded (same policy as poll_library_scan).
                    if self.env_hdr_width == 0 {
                        if let Some(root) = find_asset_root() {
                            let p = root.join("HDR_Labs/HDR_Gradient/neutral_dark_0.003.hdr");
                            if p.is_file() {
                                self.render.exposure = 1.0;
                                self.request_env_load(p);
                            }
                        }
                    }
                    return;
                }
            }
        }
        self.request_library_scan();
    }

    /// Library CONTENT changed on disk (part saved, item renamed/deleted, folder added).
    ///
    /// Clears `scanned` AND kicks the disk walk immediately — both halves are required.
    /// The forever index (`library_index.json`) carries no staleness stamp and is only
    /// rewritten when a scan installs, so a bare `self.library.scanned = false` is silently
    /// lost whenever the library panel is closed: `ensure_library_scanned()` is gated on
    /// `ui2.library.open`, and the next cold start reinstalls the pre-change index and sets
    /// `scanned = true` again — the new item stays invisible forever. Never write the bare
    /// flag; call this.
    pub(crate) fn mark_library_catalog_dirty(&mut self) {
        self.library.scanned = false;
        self.request_library_scan();
    }

    /// Force a full disk rescan and rewrite the forever index (menu / debug).
    pub(crate) fn force_library_rescan(&mut self) {
        crate::library_index_cache::invalidate_library_index();
        self.library.scanned = false;
        self.library.items.clear();
        self.library_scan_rx = None; // allow a new worker even if one was mid-flight
        self.request_library_scan();
    }

    /// Drain library right-click menu actions (find / favourite / rename / delete / …).
    pub(crate) fn handle_lib_ctx_action(&mut self, action: crate::library_panel::LibCtxAction) {
        match action {
            crate::library_panel::LibCtxAction::RefreshLibrary => {
                self.toast("Refreshing library catalog…");
                self.force_library_rescan();
            }
            crate::library_panel::LibCtxAction::RevealInExplorer { item_idx } => {
                let Some(item) = self.library.items.get(item_idx) else {
                    self.toast("Library item missing");
                    return;
                };
                let path = crate::library_panel::item_primary_path(item);
                reveal_in_explorer(&path);
                self.toast(&format!("Opened in Explorer: {}", path.display()));
            }
            crate::library_panel::LibCtxAction::ToggleFavorite { item_idx } => {
                let Some(item) = self.library.items.get_mut(item_idx) else { return; };
                let key = crate::library_panel::item_fav_key(item);
                item.favorite = !item.favorite;
                let fav = item.favorite;
                let name = item.name.clone();
                if fav {
                    self.ui2.library.favorites.insert(key);
                    self.toast(&format!("★  {}", name));
                } else {
                    self.ui2.library.favorites.remove(&key);
                    self.toast(&format!("Unstarred  {}", name));
                }
            }
            crate::library_panel::LibCtxAction::Rename { item_idx, new_name } => {
                self.lib_rename_item(item_idx, &new_name);
            }
            crate::library_panel::LibCtxAction::Delete { item_idx } => {
                self.lib_delete_item(item_idx);
            }
            crate::library_panel::LibCtxAction::Duplicate { item_idx } => {
                self.lib_duplicate_item(item_idx);
            }
            crate::library_panel::LibCtxAction::CreateGroup { item_idx } => {
                self.lib_create_group(item_idx);
            }
            crate::library_panel::LibCtxAction::ApplyToSelection { item_idx } => {
                let Some(item) = self.library.items.get(item_idx) else {
                    self.toast("Library item missing");
                    return;
                };
                let cat = item.category;
                match cat {
                    ContentCategory::Materials | ContentCategory::Textures => {
                        // Folder PBR → graph material on poly and/or SDF selection.
                        // Single textures still target an SDF prim (legacy path).
                        self.apply_library_material_to_selection(item_idx);
                    }
                    ContentCategory::HDRIs => {
                        self.apply_library_hdri(item_idx);
                    }
                    ContentCategory::MatCaps => {
                        if let Some(prim_idx) = self.active_prim_index_unified() {
                            // Matcap is global shading — still apply; selection is optional UX gate.
                            let _ = prim_idx;
                        }
                        self.apply_library_matcap(item_idx);
                    }
                    ContentCategory::Bodies => {
                        let _ = self.insert_library_body_item(item_idx, None);
                    }
                }
            }
            crate::library_panel::LibCtxAction::SetDefaultMatcap { item_idx } => {
                self.set_default_matcap_from_library(item_idx);
            }
            crate::library_panel::LibCtxAction::FolderNew { parent_tree, name } => {
                self.lib_folder_new(parent_tree.as_deref(), &name);
            }
            crate::library_panel::LibCtxAction::FolderRename { tree_path, new_name } => {
                self.lib_folder_rename(&tree_path, &new_name);
            }
            crate::library_panel::LibCtxAction::FolderDuplicate { tree_path } => {
                self.lib_folder_duplicate(&tree_path);
            }
            crate::library_panel::LibCtxAction::FolderDelete { tree_path } => {
                self.lib_folder_delete(&tree_path);
            }
            crate::library_panel::LibCtxAction::FolderReveal { tree_path } => {
                if let Some(path) = folder_tree_to_disk(&tree_path) {
                    reveal_in_explorer(&path);
                    self.toast(&format!("Opened: {}", path.display()));
                } else {
                    self.toast("Cannot map folder to disk");
                }
            }
            crate::library_panel::LibCtxAction::FolderToggleFavorite { tree_path } => {
                if self.ui2.library.bookmarks.contains(&tree_path) {
                    self.ui2.library.bookmarks.remove(&tree_path);
                    self.toast("Removed from favourites");
                } else {
                    self.ui2.library.bookmarks.insert(tree_path.clone());
                    self.toast("★ Added to favourites");
                }
            }
            crate::library_panel::LibCtxAction::AddLibraryFolder => {
                self.lib_add_extra_folder();
            }
            crate::library_panel::LibCtxAction::RemoveLibraryFolder { index } => {
                self.lib_remove_extra_folder(index);
            }
        }
    }

    /// Owner req #2 — open a native folder picker and add the chosen folder to the persisted
    /// extra-library-roots list, then rescan. Drained AFTER the panel draw (never inside a
    /// draw closure / GPU path), so a synchronous modal pick is safe here (win_texture.rs and
    /// ui2/menus.rs pick the same way). No files are copied — only the path is stored.
    fn lib_add_extra_folder(&mut self) {
        let Some(dir) = rfd::FileDialog::new()
            .set_title("Add library folder — pick a folder containing your material folders")
            .pick_folder()
        else {
            return; // cancelled
        };
        let mut folders = crate::library_folders::load();
        if folders.iter().any(|p| p == &dir) {
            self.toast("Folder already in library");
            return;
        }
        folders.push(dir.clone());
        crate::library_folders::save(&folders);
        self.toast(&format!("Added library folder: {}", dir.display()));
        // Rescan to surface its content; the panel's display copy refreshes next frame.
        self.library.scanned = false;
        self.request_library_scan();
    }

    /// Owner req #2 — remove the extra library folder at `index` and rescan.
    fn lib_remove_extra_folder(&mut self, index: usize) {
        let mut folders = crate::library_folders::load();
        if index >= folders.len() {
            return;
        }
        let removed = folders.remove(index);
        crate::library_folders::save(&folders);
        self.toast(&format!("Removed library folder: {}", removed.display()));
        self.library.scanned = false;
        self.request_library_scan();
    }

    fn lib_folder_new(&mut self, parent_tree: Option<&str>, name: &str) {
        let safe: String = name
            .chars()
            .map(|c| if r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
            .collect();
        let parent = match parent_tree {
            Some(t) => match folder_tree_to_disk(t) {
                Some(p) => p,
                None => {
                    self.toast("Cannot map parent folder");
                    return;
                }
            },
            None => match find_asset_root() {
                Some(r) => r,
                None => {
                    self.toast("assets/ not found");
                    return;
                }
            },
        };
        let dest = parent.join(&safe);
        if dest.exists() {
            self.toast("Folder already exists");
            return;
        }
        match std::fs::create_dir_all(&dest) {
            Ok(()) => {
                let _ = std::fs::write(dest.join(".gitkeep"), b"");
                if let Some(t) = parent_tree {
                    self.ui2.library.open_folders.insert(t.to_string());
                    let child = format!("{t}/{safe}");
                    self.ui2.library.sel = Some(child);
                }
                self.library.scanned = false;
                self.library.scanned = false; self.request_library_scan();
                self.toast(&format!("Created “{}”", safe));
            }
            Err(e) => self.toast(&format!("Create failed: {e}")),
        }
    }

    fn lib_folder_rename(&mut self, tree_path: &str, new_name: &str) {
        let safe: String = new_name
            .trim()
            .chars()
            .map(|c| if r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
            .collect();
        if safe.is_empty() {
            self.toast("Name cannot be empty");
            return;
        }
        let Some(path) = folder_tree_to_disk(tree_path) else {
            self.toast("Cannot map folder to disk");
            return;
        };
        if !path.is_dir() {
            self.toast("Folder not found on disk");
            return;
        }
        let Some(parent) = path.parent().map(|p| p.to_path_buf()) else {
            self.toast("Cannot rename this folder");
            return;
        };
        let dest = parent.join(&safe);
        if dest.exists() {
            self.toast("A folder with that name already exists");
            return;
        }
        match std::fs::rename(&path, &dest) {
            Ok(()) => {
                // Fix bookmarks / selection paths
                let old = tree_path.to_string();
                let parent_tree = tree_path
                    .rsplit_once('/')
                    .map(|(a, _)| a.to_string());
                let new_tree = match parent_tree {
                    Some(p) => format!("{p}/{safe}"),
                    None => safe.clone(),
                };
                if self.ui2.library.bookmarks.remove(&old) {
                    self.ui2.library.bookmarks.insert(new_tree.clone());
                }
                if self.ui2.library.sel.as_deref() == Some(old.as_str()) {
                    self.ui2.library.sel = Some(new_tree.clone());
                }
                self.ui2.library.open_folders.remove(&old);
                self.ui2.library.open_folders.insert(new_tree);
                self.library.scanned = false;
                self.library.scanned = false; self.request_library_scan();
                self.toast(&format!("Renamed → {safe}"));
            }
            Err(e) => self.toast(&format!("Rename failed: {e}")),
        }
    }

    fn lib_folder_duplicate(&mut self, tree_path: &str) {
        let Some(path) = folder_tree_to_disk(tree_path) else {
            self.toast("Cannot map folder to disk");
            return;
        };
        if !path.is_dir() {
            self.toast("Folder not found on disk");
            return;
        }
        let Some(parent) = path.parent().map(|p| p.to_path_buf()) else {
            self.toast("Cannot duplicate this folder");
            return;
        };
        let stem = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("copy");
        let mut n = 1u32;
        let dest = loop {
            let name = if n == 1 {
                format!("{stem}_copy")
            } else {
                format!("{stem}_copy{n}")
            };
            let c = parent.join(&name);
            if !c.exists() {
                break c;
            }
            n += 1;
            if n > 99 {
                self.toast("Too many copies");
                return;
            }
        };
        match copy_dir_recursive(&path, &dest) {
            Ok(()) => {
                self.library.scanned = false;
                self.library.scanned = false; self.request_library_scan();
                self.toast(&format!(
                    "Duplicated → {}",
                    dest.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("copy")
                ));
            }
            Err(e) => self.toast(&format!("Duplicate failed: {e}")),
        }
    }

    fn lib_folder_delete(&mut self, tree_path: &str) {
        let Some(path) = folder_tree_to_disk(tree_path) else {
            self.toast("Cannot map folder to disk");
            return;
        };
        // Refuse deleting category roots that are the scan roots themselves without content path
        if !path.exists() {
            self.toast("Folder not found on disk");
            return;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("folder")
            .to_string();
        let result = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match result {
            Ok(()) => {
                self.ui2.library.bookmarks.remove(tree_path);
                if self.ui2.library.sel.as_deref() == Some(tree_path) {
                    self.ui2.library.sel = None;
                }
                self.ui2.library.open_folders.remove(tree_path);
                self.library.scanned = false;
                self.library.scanned = false; self.request_library_scan();
                self.toast(&format!("Deleted “{name}”"));
            }
            Err(e) => self.toast(&format!("Delete failed: {e}")),
        }
    }

    fn lib_rename_item(&mut self, item_idx: usize, new_name: &str) {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            self.toast("Name cannot be empty");
            return;
        }
        // Sanitize for filesystem
        let safe: String = new_name
            .chars()
            .map(|c| if r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
            .collect();
        let Some(item) = self.library.items.get(item_idx) else {
            self.toast("Library item missing");
            return;
        };
        let old_key = crate::library_panel::item_fav_key(item);
        let path = crate::library_panel::item_primary_path(item);
        let is_file = path.is_file();
        let parent = path.parent().map(|p| p.to_path_buf());
        let Some(parent) = parent else {
            self.toast("Cannot rename: no parent folder");
            return;
        };
        let new_path = if is_file {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| format!(".{}", e))
                .unwrap_or_default();
            // Keep extension if user didn't type one
            let base = if safe.contains('.') {
                safe.clone()
            } else {
                format!("{}{}", safe, ext)
            };
            parent.join(base)
        } else {
            parent.join(&safe)
        };
        if new_path == path {
            self.toast("Name unchanged");
            return;
        }
        if new_path.exists() {
            self.toast("A file or folder with that name already exists");
            return;
        }
        match std::fs::rename(&path, &new_path) {
            Ok(()) => {
                // Migrate favourite key
                if self.ui2.library.favorites.remove(&old_key) {
                    self.ui2
                        .library
                        .favorites
                        .insert(new_path.to_string_lossy().into_owned());
                }
                self.library.scanned = false;
                self.library.scanned = false; self.request_library_scan();
                self.toast(&format!("Renamed → {}", safe));
            }
            Err(e) => self.toast(&format!("Rename failed: {}", e)),
        }
    }

    fn lib_delete_item(&mut self, item_idx: usize) {
        let Some(item) = self.library.items.get(item_idx) else {
            self.toast("Library item missing");
            return;
        };
        let name = item.name.clone();
        let key = crate::library_panel::item_fav_key(item);
        let path = crate::library_panel::item_primary_path(item);
        let result = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else if path.is_file() {
            std::fs::remove_file(&path)
        } else {
            // Material folders: remove folder_path if dir; else try each file
            if item.folder_path.is_dir() && !item.is_individual_file {
                std::fs::remove_dir_all(&item.folder_path)
            } else {
                let mut last_err = None;
                for (_, f) in &item.files {
                    if let Err(e) = std::fs::remove_file(f) {
                        last_err = Some(e);
                    }
                }
                match last_err {
                    Some(e) => Err(e),
                    None => Ok(()),
                }
            }
        };
        match result {
            Ok(()) => {
                self.ui2.library.favorites.remove(&key);
                self.ui2.library.selected = None;
                self.library.scanned = false;
                self.library.scanned = false; self.request_library_scan();
                self.toast(&format!("Deleted “{}”", name));
            }
            Err(e) => self.toast(&format!("Delete failed: {}", e)),
        }
    }

    fn lib_duplicate_item(&mut self, item_idx: usize) {
        let Some(item) = self.library.items.get(item_idx) else {
            self.toast("Library item missing");
            return;
        };
        let path = crate::library_panel::item_primary_path(item);
        let parent = match path.parent() {
            Some(p) => p.to_path_buf(),
            None => {
                self.toast("Cannot duplicate: no parent folder");
                return;
            }
        };
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("copy");
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e))
            .unwrap_or_default();
        let mut n = 1u32;
        let dest = loop {
            let name = if path.is_dir() {
                format!("{}_copy{}", stem, if n == 1 { String::new() } else { n.to_string() })
            } else {
                format!(
                    "{}_copy{}{}",
                    stem,
                    if n == 1 { String::new() } else { n.to_string() },
                    ext
                )
            };
            let candidate = parent.join(name);
            if !candidate.exists() {
                break candidate;
            }
            n += 1;
            if n > 99 {
                self.toast("Too many copies");
                return;
            }
        };
        let result = if path.is_dir() {
            copy_dir_recursive(&path, &dest)
        } else if path.is_file() {
            std::fs::copy(&path, &dest).map(|_| ())
        } else if item.folder_path.is_dir() && !item.is_individual_file {
            copy_dir_recursive(&item.folder_path, &dest)
        } else if let Some((_, f)) = item.files.first() {
            std::fs::copy(f, &dest).map(|_| ())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no source path",
            ))
        };
        match result {
            Ok(()) => {
                self.library.scanned = false;
                self.library.scanned = false; self.request_library_scan();
                self.toast(&format!(
                    "Duplicated → {}",
                    dest.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("copy")
                ));
            }
            Err(e) => self.toast(&format!("Duplicate failed: {}", e)),
        }
    }

    fn lib_create_group(&mut self, item_idx: usize) {
        let Some(item) = self.library.items.get(item_idx) else {
            self.toast("Library item missing");
            return;
        };
        // Group lives next to the item: parent of file, or parent of material folder.
        let path = crate::library_panel::item_primary_path(item);
        let parent = if path.is_dir() {
            path.parent().map(|p| p.to_path_buf()).unwrap_or(path.clone())
        } else {
            path.parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| item.folder_path.clone())
        };
        let mut n = 1u32;
        let dest = loop {
            let name = if n == 1 {
                "New Group".to_string()
            } else {
                format!("New Group {}", n)
            };
            let candidate = parent.join(name);
            if !candidate.exists() {
                break candidate;
            }
            n += 1;
            if n > 50 {
                self.toast("Cannot create group folder");
                return;
            }
        };
        match std::fs::create_dir_all(&dest) {
            Ok(()) => {
                // Seed an empty .gitkeep so the folder is visible / tracked
                let _ = std::fs::write(dest.join(".gitkeep"), b"");
                self.library.scanned = false;
                self.library.scanned = false; self.request_library_scan();
                let gname = dest
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("New Group");
                self.toast(&format!("Created group “{}”", gname));
            }
            Err(e) => self.toast(&format!("Create group failed: {}", e)),
        }
    }

    /// Background-load thumbnails for the given item indices (max 8 enqueue / 6 poll).
    /// Call while the library UI is open. Prefer **visible** indices (virtualized grid)
    /// so idle frames do not thrash the full library.
    ///
    /// Repaint policy (idle tax fix): only repaint when a texture just landed, or
    /// throttle while work is in flight. Failed loads (`thumb_attempted` without handle)
    /// must NOT force continuous repaint.
    pub(crate) fn library_tick_thumbs(&mut self, ctx: &egui::Context, want: &[usize]) {
        let mut enqueued = 0u32;
        for &idx in want {
            if enqueued >= 8 {
                break;
            }
            if idx >= self.library.items.len() || self.library.items[idx].thumb_attempted {
                continue;
            }
            let source_and_cache = if let Some(ref tp) = self.library.items[idx].thumb_path {
                Some((tp.clone(), None))
            } else {
                let img_path = if self.library.items[idx].is_individual_file {
                    self.library.items[idx].files.first().map(|(_, p)| p.clone())
                } else {
                    self.library.items[idx]
                        .files
                        .iter()
                        .find(|(r, _)| r == "basecolor")
                        .or_else(|| self.library.items[idx].files.first())
                        .map(|(_, p)| p.clone())
                };
                img_path.map(|p| {
                    let cache = thumb_cache_path(&p);
                    (p, Some(cache))
                })
            };
            self.library.items[idx].thumb_attempted = true;
            if let Some((path, cache_path)) = source_and_cache {
                self.thumb_bridge.request(ThumbRequest {
                    item_idx: idx,
                    source_path: path,
                    cache_path,
                });
                enqueued += 1;
            }
        }

        let results = self.thumb_bridge.poll(6);
        let mut any_loaded = false;
        for res in results {
            if res.width == 0 || res.rgba.is_empty() {
                // Failed decode — leave handle none; item stays attempted.
                continue;
            }
            if res.item_idx < self.library.items.len() {
                let size = [res.width as usize, res.height as usize];
                let ci = egui::ColorImage::from_rgba_unmultiplied(size, &res.rgba);
                let handle = ctx.load_texture(
                    format!("lib_thumb_{}", res.item_idx),
                    ci,
                    egui::TextureOptions::LINEAR,
                );
                self.library.items[res.item_idx].thumb_handle = Some(handle);
                any_loaded = true;
            }
        }
        // B3d: thumbs are UI-only — never mark_dirty(RENDER). One delayed repaint when a
        // texture lands or work remains; do NOT request_repaint() (immediate) which, paired
        // with mid-frame was_dirty, self-sustained a few-fps ping-pong.
        if any_loaded || enqueued > 0 || self.thumb_bridge.has_pending() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        // Idle with settled thumbs: no request (leave pacing to app_update).
    }
}

// ==================================================================
// Free Functions
// ==================================================================

pub(crate) fn detect_texture_role(filename: &str) -> &'static str {
    let f = filename.to_lowercase();
    if f.contains("basecolor") || f.contains("base_color") || f.contains("albedo") || f.contains("diffuse") { return "basecolor"; }
    if f.contains("normal") { return "normal"; }
    // Emission BEFORE the basecolor fallthrough. Without this branch every
    // `*_Emission.*` file hit the `"basecolor"` default at the bottom (34 files in the
    // bundled library), so the Emission channel could never bind and an emission map
    // could be picked up as a base colour. `library_item_map_files` has had the
    // ("emission"|"emissive") entries waiting the whole time.
    if f.contains("emission") || f.contains("emissive") { return "emission"; }
    // Packed ORM (UE export layout) BEFORE roughness/metallic — the filename contains both
    // words, and tagging the whole pack as "roughness" made the shader sample .r = OCCLUSION.
    // material_import splits it: G → Roughness, B → Metallic.
    if f.contains("occlusionroughnessmetallic") || f.contains("occlusion_roughness_metallic") || f.contains("_orm") { return "orm"; }
    if f.contains("roughness") { return "roughness"; }
    if f.contains("metallic") || f.contains("metalness") { return "metallic"; }
    // A real bump map is its own role. Previously unreachable, which left the
    // (MapKind::Bump, "bump") routing entry dead. SAFE: no bundled file is named *bump*.
    if f.contains("bump") { return "bump"; }
    // REVERTED 2026-08-06, same day, after a severe visual regression.
    //
    // These two ARE different roles, and collapsing them sends ambientCG's
    // `*_Displacement.*` into the Bump slot where the shader stacks it on top of the
    // normal map — real double-applied surface detail on ~97.5% of the library. But
    // splitting them routes the file to `MapKind::Displacement`, and
    // `gbuffer.wgsl:434` OFFSETS VERTICES by `sample * strength`. The install strength
    // is hardcoded `1.0` at FOUR separate sites (texture_cache.rs:145/226,
    // material_editor.rs:663/701) — zeroing only the import path left the BAKE path at
    // full strength, which shredded the geometry the moment a material re-baked.
    //
    // Splitting is still the right end state, but it needs ONE shared default-strength
    // policy for MapKind::Displacement (0.0 = wired but off) applied at every install
    // site, plus a visual check. Until then the cosmetic double-bump is the far cheaper
    // defect. Do NOT re-split this without doing the strength policy first.
    if f.contains("displacement") || f.contains("height") { return "height"; }
    if f.contains("specular") { return "specular"; }
    if f.contains("ambient_occlusion") || f.contains("_ao") { return "ao"; }
    "basecolor" // default for unknown filenames
}

#[cfg(test)]
mod role_tests {
    use super::detect_texture_role;

    /// The packed-ORM filename contains BOTH "roughness" and "metallic" — it must resolve to
    /// its own role (split G/B at import), never to "roughness" (whose .r sample is occlusion).
    #[test]
    fn packed_orm_wins_over_roughness_substring() {
        assert_eq!(detect_texture_role("rock_test_Material_OcclusionRoughnessMetallic.png"), "orm");
        assert_eq!(detect_texture_role("hero_ORM.png"), "orm");
        assert_eq!(detect_texture_role("Bricks038_1K-JPG_Roughness.jpg"), "roughness");
        assert_eq!(detect_texture_role("Metal_metallic.png"), "metallic");
        assert_eq!(detect_texture_role("Bricks038_1K-JPG_NormalGL.jpg"), "normal");
    }
}

fn role_display_name(role: &str) -> &'static str {
    match role {
        "basecolor" => "Albedo / Diffuse",
        "normal" => "Normal Maps",
        "roughness" => "Roughness Maps",
        "metallic" => "Metallic Maps",
        "orm" => "Packed ORM",
        "height" => "Height / Displacement",
        "displacement" => "Displacement", // unreachable while the split is reverted
        "bump" => "Bump",
        "emission" => "Emission",
        "specular" => "Specular Maps",
        "ao" => "Ambient Occlusion",
        _ => "Other",
    }
}

/// Locate the bundled `assets/` folder (walk parents of the exe).
/// Result is **memoized for process lifetime** — do not call a full FS walk every paint frame.
pub(crate) fn find_asset_root() -> Option<PathBuf> {
    use std::sync::OnceLock;
    static CACHED: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            let mut dir: &Path = exe.as_path();
            for _ in 0..10 {
                dir = dir.parent()?;
                let candidate = dir.join("assets");
                if candidate.is_dir() {
                    return Some(candidate);
                }
            }
            None
        })
        .clone()
}

pub(crate) fn scan_individual_images(folder: &Path, parent_name: &str, next_id: &mut u32, items: &mut Vec<ContentItem>) {
    let Ok(entries) = std::fs::read_dir(folder) else { return; };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() { continue; }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if !is_texture_map_ext(&ext) { continue; }

        let fname = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let role = detect_texture_role(&fname);

        let id = *next_id;
        *next_id += 1;
        items.push(ContentItem {
            id,
            name: fname,
            category: ContentCategory::Textures,
            folder_path: folder.to_path_buf(),
            thumb_path: None,
            files: vec![(role.into(), path)],
            thumb_handle: None,
            thumb_attempted: false,
            tags: Vec::new(),
            favorite: false,
            is_individual_file: true,
            parent_folder: parent_name.into(),
            texture_role: role_display_name(role).into(),
        });
    }
}

pub(crate) fn scan_pbr_folder(folder: &Path, next_id: &mut u32) -> Option<ContentItem> {
    let name = folder.file_name()?.to_string_lossy().to_string();
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let mut thumb_file: Option<PathBuf> = None;

    if let Ok(entries) = std::fs::read_dir(folder) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() { continue; }
            let fname = path.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            if !is_texture_map_ext(&ext) { continue; }

            // Rendered preview (e.g. "<name>_preview.png") is the tile image, never a texture map
            if fname.contains("preview") {
                thumb_file = Some(path);
                continue;
            }

            let role = detect_texture_role(&fname);
            if role != "basecolor" || !fname.contains("_mas") {
                files.push((role.into(), path));
            }
        }
    }

    if files.is_empty() { return None; }

    let id = *next_id;
    *next_id += 1;
    Some(ContentItem {
        id, name,
        category: ContentCategory::Materials,
        folder_path: folder.to_path_buf(),
        thumb_path: thumb_file, files,
        thumb_handle: None,
        thumb_attempted: false,
        tags: Vec::new(),
        favorite: false,
        is_individual_file: false,
        parent_folder: String::new(),
        texture_role: String::new(),
    })
}

pub(crate) fn scan_hdri_folder(folder: &Path, next_id: &mut u32) -> Vec<ContentItem> {
    let folder_name = match folder.file_name() {
        Some(n) => n.to_string_lossy().to_string(),
        None => return Vec::new(),
    };
    let mut named_files: Vec<(String, PathBuf)> = Vec::new();
    let mut generic_hdrs: Vec<PathBuf> = Vec::new();
    let mut display_name = folder_name.replace('_', " ");
    let mut thumb_file: Option<PathBuf> = None;
    let mut has_env_or_ref = false;

    if let Ok(entries) = std::fs::read_dir(folder) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() { continue; }
            let fname = path.file_name().unwrap_or_default().to_string_lossy().to_lowercase();

            if fname.ends_with("_env.hdr") {
                named_files.push(("env".into(), path));
                has_env_or_ref = true;
            } else if fname.ends_with(".hdr") && (fname.contains("3k") || fname.contains("ref") || fname.contains("2k")) {
                named_files.push(("ref".into(), path));
                has_env_or_ref = true;
            } else if fname.ends_with(".hdr") {
                generic_hdrs.push(path);
            } else if fname.ends_with("_8k.jpg") {
                named_files.push(("background".into(), path));
            } else if fname.contains("thumb") && fname.ends_with(".jpg") {
                thumb_file = Some(path);
            } else if fname.contains("preview") && fname.ends_with(".jpg") {
                if thumb_file.is_none() { thumb_file = Some(path); }
            } else if fname.ends_with(".ibl") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    for line in content.lines() {
                        if let Some(n) = line.strip_prefix("Name = ") {
                            let n = n.trim();
                            if !n.is_empty() { display_name = n.to_string(); }
                        }
                    }
                }
                named_files.push(("ibl".into(), path));
            }
        }
    }

    let mut items = Vec::new();

    if has_env_or_ref {
        // Standard HDR Labs folder: bundle all into one item, generic .hdr as fallback env
        let mut files = named_files;
        for p in generic_hdrs {
            files.push(("env".into(), p));
        }
        if !files.is_empty() {
            let id = *next_id; *next_id += 1;
            items.push(ContentItem {
                id, name: display_name,
                category: ContentCategory::HDRIs,
                folder_path: folder.to_path_buf(),
                thumb_path: thumb_file, files,
                thumb_handle: None, thumb_attempted: false,
                tags: Vec::new(), favorite: false,
                is_individual_file: false, parent_folder: String::new(),
                texture_role: String::new(),
            });
        }
    } else {
        // No _env/_ref: each .hdr becomes its own item (e.g. HDR_Gradient folder)
        for p in generic_hdrs {
            let fname = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            let name = fname.trim_end_matches(".hdr").replace('_', " ");
            let id = *next_id; *next_id += 1;
            items.push(ContentItem {
                id, name,
                category: ContentCategory::HDRIs,
                folder_path: folder.to_path_buf(),
                thumb_path: None,
                files: vec![("env".into(), p)],
                thumb_handle: None, thumb_attempted: false,
                tags: Vec::new(), favorite: false,
                is_individual_file: false, parent_folder: String::new(),
                texture_role: String::new(),
            });
        }
    }

    items
}


// -- Library context helpers --------------------------------------------------

/// Open the OS file manager, selecting `path` when possible (Windows Explorer).
fn reveal_in_explorer(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        if path.exists() {
            // /select,path selects the file/folder in Explorer
            let arg = format!("/select,{}", path.display());
            let _ = std::process::Command::new("explorer").arg(arg).spawn();
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = std::process::Command::new("explorer").arg(parent).spawn();
        }
    }
    #[cfg(target_os = "macos")]
    {
        if path.exists() {
            let _ = std::process::Command::new("open").args(["-R", &path.to_string_lossy()]).spawn();
        } else if let Some(parent) = path.parent() {
            let _ = std::process::Command::new("open").arg(parent).spawn();
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

/// Recursive directory copy (for material folder duplicate).
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
