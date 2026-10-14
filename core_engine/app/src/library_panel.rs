// core_engine/app/src/library_panel.rs
//
// Gate 7 — Library panel (mockup `#dock-bottom` / `#pane-lib`).
// Layout / chrome track the mockup. Content is real disk data from
// `content_library::scan_content_library` (assets/PBR, HDR_Labs, Matcaps, …)
// with real thumbnails via ThumbnailBridge — no placeholder swatches.

use crate::ui_menus::AzraContextMenu;
use eframe::egui;
use egui_phosphor::regular as ph;
use std::collections::{HashMap, HashSet};
use crate::ui_theme as t;
use crate::ui2::chrome::{self, ZoneEdge, ZoneState};
use crate::ui2::PanelKind;
use crate::{ContentCategory, ContentItem, LibraryDragPayload, LibraryState};

// ── Perf caches (owner idle-tax: overlays ~14ms was mostly library rebuild) ───
//
// 1. Folder tree + shown indices rebuild only when content / filter fingerprint changes.
// 2. Tile grid is virtualized (`ScrollArea::show_rows`) — only visible rows allocate widgets.
// 3. Thumbnail tick does not force continuous repaint once visible thumbs are settled.

fn cat_icon(cat: ContentCategory) -> &'static str {
    match cat {
        ContentCategory::Materials => ph::SPHERE,
        ContentCategory::HDRIs => ph::SUN,
        ContentCategory::Textures => ph::IMAGE,
        ContentCategory::MatCaps => ph::CIRCLE_HALF,
        ContentCategory::Bodies => ph::CUBE,
    }
}

fn leaf_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Tree path for an item: `"Materials/PBR"` or just the category label.
fn item_tree_path(item: &ContentItem) -> String {
    let cat = item.category.label();
    if item.parent_folder.is_empty() {
        cat.to_string()
    } else {
        format!("{cat}/{}", item.parent_folder)
    }
}

// ── State ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum LibView {
    Grid,
    List,
}

/// Mutable chrome for the library overlay (lives on `Ui2State`).
///
/// Two independent collapse modes (mockup `pane-lib` + `#dock-bottom`):
/// - **Pane collapse** (`collapsed`): body hidden, head remains (mockup `.pane.collapsed`).
/// - **Zone hide** (`open = false`): 6px restore strip (mockup `#dock-bottom.__hidden` /
///   `__bhide`). Drag the hsplit past min+SLOP, same as side docks.
pub struct LibraryUi {
    /// Zone visible (false → 6px restore strip at bottom of viewport hole).
    pub open: bool,
    /// Pane body collapsed — head only (independent of `open`).
    pub collapsed: bool,
    /// User-preferred height (mockup `--libh`, default 180). NOT permanently reduced
    /// when the window is short — only the *display* height is clamped to the hole.
    /// Kept across zone-hide so reopen restores size (mockup).
    pub height: f32,
    /// User-preferred folder column width (mockup `--libcolw`, default 176).
    pub col_w: f32,
    pub filter: String,
    /// Selected folder path, or `None` = "Tout" (all).
    pub sel: Option<String>,
    /// Expanded folder paths in the tree.
    pub open_folders: HashSet<String>,
    /// Bookmarked folder paths (quick-access chips).
    pub bookmarks: HashSet<String>,
    view: LibView,
    /// Thumbnail scale 0.5..3.0 (Ctrl+scroll on items).
    scale: f32,
    /// Favourites-only filter (funnel menu).
    fav_only: bool,
    /// Enabled root categories (funnel). Empty = show none.
    cats_on: HashSet<String>,
    /// In-flight tile drag (item index into LibraryState.items). Cleared on release.
    drag: Option<(usize, ContentCategory, String, String)>,
    /// Drag past min resistance (mockup SLOP=60) before zone-hide.
    hide_slop: f32,
    /// Right-click selection highlight (index into LibraryState.items).
    pub selected: Option<usize>,
    /// Favourites persisted by path key (re-applied after rescan).
    pub favorites: HashSet<String>,
    /// Action queued by context menu — app drains each frame.
    pub pending: Option<LibCtxAction>,
    /// Inline rename dialog: (item_idx, draft name).
    rename: Option<(usize, String)>,
    /// Delete confirmation dialog: item_idx.
    confirm_delete: Option<usize>,
    /// True when selection can receive a library material (poly body and/or SDF prim/body).
    pub can_apply: bool,
    /// Number of assignable SDF/poly bodies in the current selection (for multi-apply menu label).
    pub apply_target_count: usize,
    /// Grip menu/drag: open this panel kind as a floating window at an optional anchor.
    pub pending_open_float: Option<(PanelKind, Option<egui::Pos2>)>,
    /// Folder rename dialog: (tree_path, draft name).
    folder_rename: Option<(String, String)>,
    /// Folder delete confirm: tree_path.
    folder_confirm_delete: Option<String>,
    /// New-folder dialog: (parent_tree or None for assets root, draft name).
    folder_new: Option<(Option<String>, String)>,
    /// Configured extra library root folders (owner req #2) — a display copy the app
    /// refreshes each frame from `library_folders::load()`, so the grip menu can list them
    /// with remove buttons without touching the filesystem in the draw path.
    pub extra_folders: Vec<std::path::PathBuf>,
    // ── Draw-path caches (invalidated by fingerprint, not every frame) ───────
    /// Content fingerprint this folder index was built for: `(items.len(), next_item_id)`.
    folder_fp: (usize, u32),
    /// Immediate children of each tree path (from last folder rebuild).
    folder_children: HashMap<String, Vec<String>>,
    /// Item count under each tree path (inclusive).
    folder_counts: HashMap<String, usize>,
    /// Filter fingerprint for `shown` cache.
    shown_fp: ShownCacheFp,
    /// Indices into `LibraryState.items` matching folder/filter/fav/cats.
    shown: Vec<usize>,
    /// Subset of `shown` currently in the items scroll viewport (thumb load budget).
    pub visible_for_thumbs: Vec<usize>,
    /// True after a scan install until favorites are written onto items once.
    pub favs_need_sync: bool,
    /// Flattened open-folder tree rows for virtualization (Gate 2 fix: 2k+ dirs under
    /// expanded categories was allocating every folder_row every frame → 500–900ms).
    tree_rows: Vec<TreeRow>,
    /// Fingerprint for `tree_rows` (content + which folders are open + bookmarks).
    tree_fp: TreeCacheFp,
    /// CONSOLE_AUDIT C1: incremental `shown` rebuild cursor (items index).
    /// `None` = idle/complete; `Some(i)` = resume scan at `i` for `shown_building_fp`.
    shown_build_at: Option<usize>,
    /// Target fingerprint for the in-progress shown rebuild.
    shown_building_fp: Option<ShownCacheFp>,
}

/// One row in the virtualized folder tree (DFS of open branches only).
#[derive(Clone)]
struct TreeRow {
    /// `None` = the synthetic “All” root.
    path: Option<String>,
    depth: usize,
    is_leaf: bool,
    is_open: bool,
    icon: &'static str,
    label: String,
    count: usize,
    bookmarked: bool,
    show_bookmark: bool,
}

#[derive(Clone, Default, PartialEq, Eq)]
struct TreeCacheFp {
    content: (usize, u32),
    /// Sorted open folder paths.
    open: Vec<String>,
    /// Sorted bookmark paths (affects star icon).
    bookmarks: Vec<String>,
}

/// Fingerprint of the inputs that decide `shown` (folder / filter / fav / cats + content).
#[derive(Clone, Default, PartialEq, Eq)]
struct ShownCacheFp {
    content: (usize, u32),
    filter: String,
    sel: Option<String>,
    fav_only: bool,
    /// Sorted category labels currently enabled (stable compare).
    cats: Vec<String>,
    /// Favorites count — toggle fav must refresh fav-only filter + star flags in list.
    fav_n: usize,
}

impl Default for LibraryUi {
    fn default() -> Self {
        let mut open_folders = HashSet::new();
        for c in ContentCategory::all() {
            open_folders.insert(c.label().to_string());
        }
        let cats_on: HashSet<String> = ContentCategory::all()
            .iter()
            .map(|c| c.label().to_string())
            .collect();
        Self {
            open: true,
            collapsed: false,
            height: 180.0,
            col_w: 176.0,
            filter: String::new(),
            sel: None,
            open_folders,
            bookmarks: HashSet::new(),
            view: LibView::Grid,
            scale: 1.0,
            fav_only: false,
            cats_on,
            drag: None,
            hide_slop: 0.0,
            selected: None,
            favorites: HashSet::new(),
            pending: None,
            rename: None,
            confirm_delete: None,
            can_apply: false,
            apply_target_count: 0,
            pending_open_float: None,
            folder_rename: None,
            folder_confirm_delete: None,
            folder_new: None,
            extra_folders: Vec::new(),
            folder_fp: (0, 0),
            folder_children: HashMap::new(),
            folder_counts: HashMap::new(),
            shown_fp: ShownCacheFp::default(),
            shown: Vec::new(),
            visible_for_thumbs: Vec::new(),
            favs_need_sync: true,
            tree_rows: Vec::new(),
            tree_fp: TreeCacheFp::default(),
            shown_build_at: None,
            shown_building_fp: None,
        }
    }
}

/// CONSOLE_AUDIT C1: max wall time spent scanning items into `shown` per frame.
/// Forced full rebuilds (scan install / filter change) never monopolize a frame.
const MAX_LIB_SHOWN_MS_PER_FRAME: f32 = 4.0;

impl LibraryUi {
    /// Call after scan install / rename / delete so folder + shown caches rebuild next draw.
    pub fn invalidate_content_caches(&mut self) {
        self.folder_fp = (usize::MAX, u32::MAX);
        self.shown_fp = ShownCacheFp::default();
        self.shown.clear();
        self.folder_children.clear();
        self.folder_counts.clear();
        self.visible_for_thumbs.clear();
        self.tree_rows.clear();
        self.tree_fp = TreeCacheFp::default();
        self.favs_need_sync = true;
    }
}

/// Drop of a library tile onto the viewport (outside the library rect).
pub struct LibDrop {
    pub item_idx: usize,
    pub category: ContentCategory,
    pub name: String,
    pub folder: String,
    pub screen_pos: egui::Pos2,
}

/// Deferred right-click menu action (handled by the app after draw).
#[derive(Clone)]
pub enum LibCtxAction {
    /// Open OS file explorer at the item’s path (select file when possible).
    RevealInExplorer { item_idx: usize },
    ToggleFavorite { item_idx: usize },
    /// Rename on disk; `new_name` is the user-confirmed display/base name.
    Rename { item_idx: usize, new_name: String },
    Delete { item_idx: usize },
    Duplicate { item_idx: usize },
    /// Create a new folder (group) next to this item’s parent folder.
    CreateGroup { item_idx: usize },
    /// Apply material / texture / matcap / HDRI (category decides).
    ApplyToSelection { item_idx: usize },
    /// Persist this matcap as the startup / default viewport matcap.
    SetDefaultMatcap { item_idx: usize },
    // ── Folder tree ──────────────────────────────────────────────────────────
    /// Create a subfolder under `parent_tree` (`None` = assets root / “All”).
    FolderNew { parent_tree: Option<String>, name: String },
    FolderRename { tree_path: String, new_name: String },
    FolderDuplicate { tree_path: String },
    FolderDelete { tree_path: String },
    FolderReveal { tree_path: String },
    FolderToggleFavorite { tree_path: String },
    // ── User-configurable extra library root folders (owner req #2) ───────────
    /// Open a native folder picker and add the chosen folder as an extra library root.
    AddLibraryFolder,
    /// Remove the extra library folder at this index in the configured list.
    RemoveLibraryFolder { index: usize },
    /// Drop forever index and re-walk disk (after dropping new assets outside the app).
    RefreshLibrary,
}

/// Map library tree path (`Materials/PBR/Concrete`) → disk path under `assets/`.
pub fn folder_tree_to_disk(tree: &str) -> Option<std::path::PathBuf> {
    let root = crate::content_library::find_asset_root()?;
    let segs: Vec<&str> = tree.split('/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return Some(root);
    }
    let (base, rest): (std::path::PathBuf, &[&str]) = match segs[0] {
        "Materials" => {
            // UI: Materials[/PBR[/…]] → disk: assets/PBR[/…]
            if segs.len() >= 2 && segs[1] == "PBR" {
                (root.join("PBR"), &segs[2..])
            } else {
                (root.join("PBR"), &segs[1..])
            }
        }
        "HDRIs" => {
            if segs.len() >= 2 && segs[1] == "HDR_Labs" {
                (root.join("HDR_Labs"), &segs[2..])
            } else {
                (root.join("HDR_Labs"), &segs[1..])
            }
        }
        "Textures" => (root.join("Textures"), &segs[1..]),
        "MatCaps" => {
            if segs.len() >= 2 && segs[1].eq_ignore_ascii_case("Matcaps") {
                (root.join("Matcaps"), &segs[2..])
            } else {
                (root.join("Matcaps"), &segs[1..])
            }
        }
        _ => return None,
    };
    let mut p = base;
    for s in rest {
        p = p.join(s);
    }
    Some(p)
}

/// Stable favorites key (survives rescan).
pub fn item_fav_key(item: &ContentItem) -> String {
    if let Some((_, p)) = item.files.first() {
        p.to_string_lossy().into_owned()
    } else {
        item.folder_path
            .join(&item.name)
            .to_string_lossy()
            .into_owned()
    }
}

/// Primary path to reveal / rename / delete for an item.
pub fn item_primary_path(item: &ContentItem) -> std::path::PathBuf {
    // Material folders live in `folder_path` as the material dir itself.
    if !item.is_individual_file && item.category == ContentCategory::Materials {
        return item.folder_path.clone();
    }
    if let Some((_, p)) = item.files.first() {
        return p.clone();
    }
    item.folder_path.clone()
}

/// Absolute min/max for the user-preferred height (content_library / mockup).
const LIB_H_MIN: f32 = 100.0;
const LIB_H_MAX: f32 = 600.0;
const LIB_COL_MIN: f32 = 100.0;
const LIB_COL_MAX: f32 = 320.0;
/// Mockup `SLOP=60` — drag past min this far before zone-hide.
const LIB_ZONE_SLOP: f32 = 60.0;
/// Zone-hide restore strip (mockup `#dock-bottom.__hidden{height:6px}`).
const LIB_STRIP_H: f32 = 6.0;

/// Display height for this frame: preferred clamped into the live hole.
/// Prefer is kept intact so growing the window restores the user's size.
pub fn display_height(lib: &LibraryUi, hole_h: f32) -> f32 {
    if !lib.open {
        return LIB_STRIP_H;
    }
    // Pane collapse: head only (mockup `.pane.collapsed` — body hidden).
    if lib.collapsed {
        return t::H_PANEL_HEADER;
    }
    // Leave at least ~15% of the hole for the 3D scene.
    let max_h = (hole_h * 0.85).clamp(LIB_H_MIN, LIB_H_MAX);
    let min_h = LIB_H_MIN.min(max_h);
    lib.height.clamp(min_h, max_h)
}

/// Vertical space the library claims from the bottom of the viewport hole
/// (panel + 5px hsplit when open, 6px restore strip when zone-hidden).
pub fn bottom_inset(lib: &LibraryUi, hole_h: f32) -> f32 {
    if lib.open {
        // Pane-collapsed still has a head band; no hsplit when fully open body
        // is gone (head only) — still no resize strip when zone-hidden.
        let h = display_height(lib, hole_h);
        if lib.collapsed {
            h // head only, no hsplit above
        } else {
            h + t::SEAM_W
        }
    } else {
        LIB_STRIP_H
    }
}

/// Scene rect = viewport hole minus the library zone.
pub fn scene_rect(viewport_hole: egui::Rect, lib: &LibraryUi) -> egui::Rect {
    let cut = bottom_inset(lib, viewport_hole.height());
    let bottom = (viewport_hole.bottom() - cut).max(viewport_hole.top() + 40.0);
    egui::Rect::from_min_max(
        viewport_hole.left_top(),
        egui::pos2(viewport_hole.right(), bottom),
    )
}

/// Folder column width that always fits in `body_w` (items keep ≥80px).
fn display_col_w(lib: &LibraryUi, body_w: f32) -> f32 {
    let seam = t::SEAM_W;
    let items_min = 80.0;
    let max_for_body = (body_w - seam - items_min).max(60.0);
    let max_w = max_for_body.min(LIB_COL_MAX);
    let min_w = LIB_COL_MIN.min(max_w);
    lib.col_w.clamp(min_w, max_w)
}

// ── Draw ─────────────────────────────────────────────────────────────────────

/// Draw library **inside CentralPanel** (same layer as the viewport).
/// Never uses egui Area/Middle — that painted over Properties (owner screenshots).
///
/// **Horizontal bounds = CentralPanel cell only** (`ui.max_rect()`). The Model
/// column is exactly that cell after L/R SidePanels; using a wider external hole
/// was the "library under Properties" class.
/// Returns `(pointer_over_library, optional_drop)`.
pub fn draw_in_central(
    ui: &mut egui::Ui,
    lib: &mut LibraryUi,
    content: &mut LibraryState,
    _viewport_hole: egui::Rect,
) -> (bool, Option<LibDrop>) {
    let mut drop_ev: Option<LibDrop> = None;
    lib.height = lib.height.clamp(LIB_H_MIN, LIB_H_MAX);
    lib.col_w = lib.col_w.clamp(LIB_COL_MIN, LIB_COL_MAX);
    lib.scale = lib.scale.clamp(0.5, 3.0);

    // GROUND TRUTH: CentralPanel max_rect is already between the docks.
    // Do not trust a wider external hole — that is how library crept under Properties.
    let hole = ui.max_rect();
    ui.set_clip_rect(hole);
    if hole.width() < 40.0 || hole.height() < 20.0 {
        return (false, None);
    }

    let mut over = false;
    let ptr = ui.ctx().pointer_latest_pos().unwrap_or(egui::Pos2::new(-1.0, -1.0));
    let ui_ctx = ui.ctx().clone();

    // Zone-hidden: 6px restore strip only (O2). Do NOT rebuild folder/shown caches while
    // closed — that was free work on every idle frame (overlays bucket mis-label).
    if !lib.open {
        lib.visible_for_thumbs.clear();
        let strip = egui::Rect::from_min_max(
            egui::pos2(hole.left(), hole.bottom() - LIB_STRIP_H),
            hole.right_bottom(),
        );
        ui.allocate_ui_at_rect(strip, |ui| {
            ui.set_min_size(strip.size());
            ui.set_max_size(strip.size());
            ui.set_clip_rect(strip);
            let (r, resp) = ui.allocate_exact_size(strip.size(), egui::Sense::click_and_drag());
            ui.painter().rect_filled(r, 0.0, t::SEAM);
            if resp.hovered() || resp.dragged() {
                ui.painter().rect_filled(r, 0.0, t::ACCENT);
                ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeVertical);
            }
            if resp.clicked() {
                lib.open = true;
                lib.collapsed = false;
                lib.hide_slop = 0.0;
            } else if resp.dragged() && resp.drag_delta().y < 0.0 {
                lib.open = true;
                lib.collapsed = false;
                lib.hide_slop = 0.0;
                lib.height = (lib.height.max(LIB_H_MIN) - resp.drag_delta().y)
                    .clamp(LIB_H_MIN, LIB_H_MAX);
            }
            over = r.contains(ptr);
        });
        return (over, None);
    }

    // Open body: rebuild folder / shown / tree caches only when fingerprints change.
    let _t_ensure = std::time::Instant::now();
    prepare_content(lib, content);
    let ensure_ms = _t_ensure.elapsed().as_secs_f32() * 1000.0;
    lib.visible_for_thumbs.clear();

    let h = display_height(lib, hole.height());
    let rect = egui::Rect::from_min_max(
        egui::pos2(hole.left(), hole.bottom() - h),
        hole.right_bottom(),
    );

    // hsplit (pointer-driven chrome) — top edge of library band
    if !lib.collapsed {
        let mut zone = ZoneState {
            open: true,
            size: lib.height,
            min: LIB_H_MIN,
            max: LIB_H_MAX,
            hide_slop: lib.hide_slop,
            edge: ZoneEdge::Bottom,
        };
        let sr = chrome::draw_resize_seam(&ui_ctx, rect, &mut zone, "lib");
        if sr.resized || sr.hidden {
            lib.height = zone.size;
            lib.open = zone.open;
            lib.hide_slop = zone.hide_slop;
        }
        if sr.hovered {
            over = true;
        }
    }

    // Panel body — hard clip to hole
    let _t_paint = std::time::Instant::now();
    ui.allocate_ui_at_rect(rect, |ui| {
        ui.set_min_size(rect.size());
        ui.set_max_size(rect.size());
        ui.set_clip_rect(rect.intersect(hole));
        let panel_r = rect;
        ui.painter().rect_filled(panel_r, 0.0, t::SURFACE_PANEL);
        ui.painter().hline(
            panel_r.x_range(),
            panel_r.top() + 0.5,
            egui::Stroke::new(1.0, t::BORDER),
        );
        chrome::apply_panel_scroll_style(ui);
        let head_h = t::H_PANEL_HEADER;
        draw_head(ui, lib, head_h);
        if !lib.collapsed {
            let info_h = 24.0;
            let body_h = (panel_r.height() - head_h - info_h).max(40.0);
            draw_body(ui, lib, content, body_h);
            draw_info(ui, lib, content, info_h);
        }
        if panel_r.contains(ptr) {
            over = true;
        }
    });
    let paint_ms = _t_paint.elapsed().as_secs_f32() * 1000.0;
    // CONSOLE_AUDIT B4: 1 ms gate flooded the console every frame with the library open.
    // Stall-hunt threshold is 8 ms (mirrors [Overlays]); AZRA_PERF_SPAM restores 1 ms.
    let lib_gate = if crate::perf_spam_on() { 1.0 } else { 8.0 };
    if ensure_ms + paint_ms > lib_gate {
        eprintln!(
            "[LIB] ensure={:.1}ms paint={:.1}ms tree_rows={} shown={} items={} | {}",
            ensure_ms,
            paint_ms,
            lib.tree_rows.len(),
            lib.shown.len(),
            content.items.len(),
            crate::profiling::perf_stamp()
        );
    }

    // Rename / delete confirmation dialogs (above panel chrome).
    draw_lib_dialogs(ui.ctx(), lib, content);

    // ── drag ghost + drop (only onto the scene above the library) ──
    if let Some((item_idx, category, ref name, ref folder)) = lib.drag.clone() {
        if let Some(pos) = ui.ctx().pointer_latest_pos() {
            egui::Area::new(egui::Id::new("v2_lib_drag_ghost"))
                .order(egui::Order::Tooltip)
                .fixed_pos(pos + egui::vec2(14.0, 14.0))
                .interactable(false)
                .show(ui.ctx(), |ui| {
                    egui::Frame::none()
                        .fill(t::SURFACE_CARD)
                        .rounding(4.0)
                        .stroke(egui::Stroke::new(1.0, t::ACCENT))
                        .inner_margin(egui::Margin::symmetric(8, 4))
                        .show(ui, |ui| {
                            let tip = if category == crate::ContentCategory::Bodies {
                                format!("Place “{name}”  · Ctrl surface · scroll resize")
                            } else {
                                format!("Apply “{name}”")
                            };
                            ui.label(
                                egui::RichText::new(tip)
                                    .size(11.0)
                                    .color(t::TEXT),
                            );
                        });
                });
            // Keep engine drag payload in sync for handle_library_drag_drop.
            egui::DragAndDrop::set_payload(
                ui.ctx(),
                LibraryDragPayload {
                    item_idx,
                    category,
                },
            );
            if ui.ctx().input(|i| i.pointer.any_released()) {
                let over_lib = rect.contains(pos);
                let scene = scene_rect(hole, lib);
                if scene.contains(pos) && !over_lib {
                    drop_ev = Some(LibDrop {
                        item_idx,
                        category,
                        name: name.clone(),
                        folder: folder.clone(),
                        screen_pos: pos,
                    });
                }
                lib.drag = None;
            }
        } else if ui.ctx().input(|i| i.pointer.any_released()) {
            lib.drag = None;
        }
    }

    (over || rect.contains(ptr), drop_ev)
}

/// Draw Library content inside the shared dock/float panel chrome.
pub(crate) fn draw_in_pane(
    ui: &mut egui::Ui,
    lib: &mut LibraryUi,
    content: &mut LibraryState,
) {
    prepare_content(lib, content);
    apply_lib_scroll_style(ui);
    let info_h = 24.0;
    let body_h = (ui.available_height() - info_h).max(40.0);
    draw_body(ui, lib, content, body_h);
    draw_info(ui, lib, content, info_h);
    draw_lib_dialogs(ui.ctx(), lib, content);
}

fn prepare_content(lib: &mut LibraryUi, content: &mut LibraryState) {
    if lib.favs_need_sync {
        for item in content.items.iter_mut() {
            item.favorite = lib.favorites.contains(&item_fav_key(item));
        }
        lib.favs_need_sync = false;
        lib.shown_fp = ShownCacheFp::default();
        lib.shown_build_at = None;
        lib.shown_building_fp = None;
    }
    ensure_folder_index(lib, content);
    ensure_shown(lib, content);
    ensure_tree_rows(lib, content);
    lib.visible_for_thumbs.clear();
}

/// Back-compat name used by older call sites.
pub fn draw_overlay(
    ui_ctx: &egui::Context,
    lib: &mut LibraryUi,
    viewport_hole: egui::Rect,
) -> (bool, Option<LibDrop>) {
    // Prefer CentralPanel path; this Context-only path only runs the resize
    // seam when no Ui is available (should not be used for paint).
    let _ = (ui_ctx, lib, viewport_hole);
    (false, None)
}

/// Mockup scrollbar metrics locally (same as global theme — belt & braces for Area).
fn apply_lib_scroll_style(ui: &mut egui::Ui) {
    // Force solid CONTROL thumb — never light fg_stroke (see ui_theme::apply_style).
    ui.style_mut().spacing.scroll = egui::style::ScrollStyle {
        floating: false,
        bar_width: 7.0,
        handle_min_length: 20.0,
        bar_inner_margin: 3.0,
        bar_outer_margin: 3.0,
        floating_width: 2.0,
        floating_allocated_width: 0.0,
        foreground_color: false,
        dormant_background_opacity: 1.0,
        active_background_opacity: 1.0,
        interact_background_opacity: 1.0,
        dormant_handle_opacity: 1.0,
        active_handle_opacity: 1.0,
        interact_handle_opacity: 1.0,
    };
    ui.style_mut().visuals.widgets.inactive.bg_fill = t::SURFACE_CONTROL;
    ui.style_mut().visuals.widgets.hovered.bg_fill = t::BORDER_HOVER;
    ui.style_mut().visuals.widgets.active.bg_fill = t::ACCENT_DIM;
    ui.style_mut().visuals.extreme_bg_color = t::SURFACE_WELL;
}

// ── Head ─────────────────────────────────────────────────────────────────────

/// Paint grip + open menu (click/RMB) and detach drag. Returns grip response.
fn lib_grip_widget(ui: &mut egui::Ui, lib: &mut LibraryUi) -> egui::Response {
    const HIT: f32 = 26.0;
    let (rect, grip) =
        ui.allocate_exact_size(egui::vec2(HIT, HIT), egui::Sense::click_and_drag());
    let hot = grip.hovered() || grip.is_pointer_button_down_on();
    if hot {
        ui.painter()
            .rect_filled(rect, t::RADIUS_CONTROL, t::SURFACE_CONTROL);
        ui.output_mut(|o| {
            o.cursor_icon = if grip.dragged() {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            };
        });
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        ph::DOTS_SIX_VERTICAL,
        egui::FontId::proportional(15.0),
        if hot { t::ACCENT_LIGHT } else { t::TEXT_FAINT },
    );

    // Right-click: egui context menu (anchors correctly even at screen bottom).
    grip.azra_context_menu(|ui| {
        lib_grip_menu_body(ui, lib);
    });

    // Left-click: open popup *above* the grip (library sits at the bottom of the
    // window — popup_below would open off-screen and look like "no menu").
    let popup_id = ui.make_persistent_id("v2_lib_grip_menu");
    if grip.clicked() {
        ui.memory_mut(|m| m.open_popup(popup_id));
    }
    egui::containers::old_popup::popup_above_or_below_widget(
        ui,
        popup_id,
        &grip,
        egui::AboveOrBelow::Above,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            lib_grip_menu_body(ui, lib);
        },
    );
    if grip.drag_stopped() {
        if let Some(pos) = grip.interact_pointer_pos() {
            lib.pending_open_float = Some((PanelKind::Library, Some(pos)));
        }
    }
    grip
}

fn lib_grip_menu_body(ui: &mut egui::Ui, lib: &mut LibraryUi) {
    ui.set_min_width(200.0);
    let cl = if lib.collapsed { "Expand" } else { "Collapse" };
    if ui
        .button(format!(
            "{}  {}",
            if lib.collapsed {
                ph::CARET_DOWN
            } else {
                ph::CARET_UP
            },
            cl
        ))
        .clicked()
    {
        lib.collapsed = !lib.collapsed;
        ui.close_menu();
    }
    if ui
        .button(format!("{}  Hide library zone", ph::MINUS))
        .on_hover_text("Collapse to a 6px restore strip at the bottom")
        .clicked()
    {
        lib.height = lib.height.max(LIB_H_MIN);
        lib.open = false;
        lib.hide_slop = 0.0;
        ui.close_menu();
    }
    if ui
        .button(format!("{}  Refresh library", ph::ARROWS_CLOCKWISE))
        .on_hover_text(
            "Re-scan disk and rewrite the forever catalog cache \
             (%LOCALAPPDATA%\\\\Azra\\\\library_index.json). \
             Use after adding files outside the app.",
        )
        .clicked()
    {
        lib.pending = Some(LibCtxAction::RefreshLibrary);
        ui.close_menu();
    }
    ui.separator();
    // ── Library folders (owner req #2) ────────────────────────────────────────
    ui.label(
        egui::RichText::new("LIBRARY FOLDERS")
            .size(9.5)
            .color(t::TEXT_SECTION)
            .strong(),
    );
    ui.label(
        egui::RichText::new("Scanned in addition to the built-in library")
            .size(10.0)
            .color(t::TEXT_FAINT),
    );
    if lib.extra_folders.is_empty() {
        ui.label(
            egui::RichText::new("None added")
                .size(10.0)
                .italics()
                .color(t::TEXT_FAINT),
        );
    } else {
        // Snapshot the paths so we can mutate `lib.pending` inside the loop.
        let folders: Vec<String> = lib
            .extra_folders
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        for (idx, disp) in folders.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new(ph::X).size(11.0).color(t::TEXT_DIM),
                        )
                        .sense(egui::Sense::click()),
                    )
                    .on_hover_text("Remove this library folder")
                    .clicked()
                {
                    lib.pending = Some(LibCtxAction::RemoveLibraryFolder { index: idx });
                    ui.close_menu();
                }
                ui.label(
                    egui::RichText::new(disp)
                        .size(10.0)
                        .color(t::TEXT_DIM),
                )
                .on_hover_text(disp.as_str());
            });
        }
    }
    if ui
        .button(format!("{}  Add folder…", ph::PLUS))
        .on_hover_text("Pick a folder that contains your material folders")
        .clicked()
    {
        lib.pending = Some(LibCtxAction::AddLibraryFolder);
        ui.close_menu();
    }
    ui.separator();
    ui.label(
        egui::RichText::new("OPEN PANEL")
            .size(9.5)
            .color(t::TEXT_SECTION)
            .strong(),
    );
    ui.label(
        egui::RichText::new("Float a window")
            .size(10.0)
            .color(t::TEXT_FAINT),
    );
    for k in PanelKind::all() {
        let icon = match k {
            PanelKind::Outliner => ph::LIST,
            PanelKind::Properties => ph::SLIDERS,
            PanelKind::ToolsSdf
            | PanelKind::ToolsPoly
            | PanelKind::ToolsSculpt
            | PanelKind::ToolsNurbs => ph::WRENCH,
            PanelKind::ToolParams => ph::GEAR,
            PanelKind::Library => ph::BOOK,
            PanelKind::Image => ph::IMAGE,
            PanelKind::History => ph::CLOCK_COUNTER_CLOCKWISE,
            PanelKind::UvLayout => ph::GRID_FOUR,
            PanelKind::NodeEditor => ph::GRAPH,
            PanelKind::Channels => ph::FADERS,
            PanelKind::WireSettings => ph::BOUNDING_BOX,
            PanelKind::SolidSettings => ph::CUBE,
            PanelKind::MaterialSettings => ph::CIRCLE_HALF,
            PanelKind::PbrSettings => ph::SUN,
            PanelKind::MatCapSettings => ph::SPHERE,
            PanelKind::PtSettings => ph::SPARKLE,
            PanelKind::EnvSettings => ph::GLOBE,
            PanelKind::RenderSettingsGlobal => ph::SLIDERS,
        };
        let label = if k == PanelKind::Library {
            format!("{}  {}  (detach)", icon, k.label())
        } else {
            format!("{}  {}", icon, k.label())
        };
        let r = ui.button(label);
        if r.clicked() {
            lib.pending_open_float = Some((k, None));
            ui.close_menu();
        }
    }
    ui.separator();
    ui.label(
        egui::RichText::new("CHANGE WINDOW")
            .size(9.5)
            .color(t::TEXT_SECTION)
            .strong(),
    );
    for k in [
        PanelKind::NodeEditor,
        PanelKind::Image,
        PanelKind::UvLayout,
        PanelKind::History,
        PanelKind::Channels,
    ] {
        let icon = match k {
            PanelKind::NodeEditor => ph::GRAPH,
            PanelKind::Image => ph::IMAGE,
            PanelKind::UvLayout => ph::GRID_FOUR,
            PanelKind::History => ph::CLOCK_COUNTER_CLOCKWISE,
            PanelKind::Channels => ph::FADERS,
            _ => ph::SQUARE,
        };
        if ui
            .button(format!("{}  {}", icon, k.label()))
            .on_hover_text(format!("Open {} as a floating panel", k.label()))
            .clicked()
        {
            lib.pending_open_float = Some((k, None));
            ui.close_menu();
        }
    }
}

fn draw_head(ui: &mut egui::Ui, lib: &mut LibraryUi, head_h: f32) {
    // Mockup .pane-head: grip · Library · sub · lib-bar (cats | quick | Grid/List)
    let head_rect = {
        let r = ui.available_rect_before_wrap();
        egui::Rect::from_min_size(r.min, egui::vec2(r.width(), head_h))
    };
    let _ = ui.allocate_exact_size(head_rect.size(), egui::Sense::hover());

    ui.allocate_ui_at_rect(head_rect, |ui| {
        ui.set_clip_rect(head_rect); // chips must not expand past the hole width
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.add_space(6.0);
            // Grip (⣿) — solid hit target; LMB popup + RMB context_menu (dock parity).
            let grip = lib_grip_widget(ui, lib);
            ui.label(
                egui::RichText::new("Library")
                    .size(12.0)
                    .strong()
                    .color(t::TEXT),
            );
            // .sub — leaf of selection, or "Tout"
            let sub = lib
                .sel
                .as_deref()
                .map(leaf_name)
                .unwrap_or("Tout");
            ui.label(
                egui::RichText::new(sub)
                    .size(11.0)
                    .color(t::SDF)
                    .strong(),
            );

            // Category chips (mockup #lib-cats)
            ui.add_space(4.0);
            let root_owned: Option<String> = lib
                .sel
                .as_ref()
                .and_then(|p| p.split('/').next().map(|s| s.to_string()));
            for &cat in ContentCategory::all() {
                let label = cat.label();
                let on = root_owned.as_deref() == Some(label);
                if lib_chip(ui, Some(cat_icon(cat)), label, on) {
                    lib.sel = Some(label.to_string());
                    lib.open_folders.insert(label.to_string());
                }
            }

            // vsep
            ui.add_space(3.0);
            let (sep, _) = ui.allocate_exact_size(egui::vec2(1.0, 16.0), egui::Sense::hover());
            ui.painter().rect_filled(sep, 0.0, t::SEAM);
            ui.add_space(3.0);

            // Quick bookmarks
            if lib.bookmarks.is_empty() {
                ui.label(
                    egui::RichText::new("★ pin in tree")
                        .size(10.0)
                        .italics()
                        .color(t::TEXT_FAINT),
                );
            } else {
                let bms: Vec<String> = {
                    let mut v: Vec<_> = lib.bookmarks.iter().cloned().collect();
                    v.sort();
                    v
                };
                for p in bms {
                    let on = lib.sel.as_deref() == Some(p.as_str());
                    let label = leaf_name(&p);
                    if lib_chip(ui, None, &format!("★ {label}"), on) {
                        // Reveal ancestors + select
                        let segs: Vec<&str> = p.split('/').collect();
                        for k in 1..segs.len() {
                            lib.open_folders.insert(segs[..k].join("/"));
                        }
                        lib.sel = Some(p);
                    }
                }
            }

            // Right: Grid / List + collapse affordances (mockup grip menu subset)
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                // Zone hide → 6px strip (mockup "Masquer le panneau" / drag past min+SLOP).
                if ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new(ph::MINUS)
                                .size(13.0)
                                .color(t::TEXT_DIM),
                        )
                        .sense(egui::Sense::click()),
                    )
                    .on_hover_text("Hide library zone (6px restore strip)")
                    .clicked()
                {
                    lib.height = lib.height.max(LIB_H_MIN);
                    lib.open = false;
                    lib.hide_slop = 0.0;
                }
                ui.add_space(4.0);
                // Pane collapse: body hidden, head remains (mockup `.pane.collapsed`).
                let pane_icon = if lib.collapsed {
                    ph::CARET_DOWN
                } else {
                    ph::CARET_UP
                };
                let pane_tip = if lib.collapsed {
                    "Expand library body"
                } else {
                    "Collapse library body (keep header)"
                };
                if ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new(pane_icon)
                                .size(13.0)
                                .color(t::TEXT_DIM),
                        )
                        .sense(egui::Sense::click()),
                    )
                    .on_hover_text(pane_tip)
                    .clicked()
                {
                    lib.collapsed = !lib.collapsed;
                }
                ui.add_space(4.0);
                if !lib.collapsed {
                    let list_on = lib.view == LibView::List;
                    if lib_chip(ui, Some(ph::STACK), "List", list_on) {
                        lib.view = LibView::List;
                    }
                    let grid_on = lib.view == LibView::Grid;
                    if lib_chip(ui, Some(ph::GRID_FOUR), "Grid", grid_on) {
                        lib.view = LibView::Grid;
                    }
                }
            });
        });
    });

    // Bottom edge of head — continuous surface (no hard rule in mockup pane-head,
    // but body needs a clean break from the toolbar). A 1px seam is the app language.
    ui.painter().hline(
        head_rect.x_range(),
        head_rect.bottom() - 0.5,
        egui::Stroke::new(1.0, t::SEAM),
    );
}

/// Mockup `.lib-chip` — height 22, pad 0 9, r5, control fill, accent border when on.
fn lib_chip(ui: &mut egui::Ui, icon: Option<&str>, label: &str, on: bool) -> bool {
    let text = if let Some(ic) = icon {
        format!("{ic} {label}")
    } else {
        label.to_string()
    };
    let galley = ui.fonts(|f| {
        f.layout_no_wrap(
            text.clone(),
            egui::FontId::proportional(10.5),
            t::TEXT,
        )
    });
    let w = galley.size().x + 18.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 22.0), egui::Sense::click());
    let border = if on {
        t::ACCENT
    } else if resp.hovered() {
        t::BORDER_HOVER
    } else {
        t::SEAM
    };
    let fg = if on {
        t::ACCENT_LIGHT
    } else if resp.hovered() {
        t::TEXT
    } else {
        t::TEXT_SECONDARY
    };
    ui.painter().rect_filled(rect, 5.0, t::SURFACE_CONTROL);
    ui.painter()
        .rect_stroke(rect, 5.0, egui::Stroke::new(1.0, border), egui::StrokeKind::Middle);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        &text,
        egui::FontId::proportional(10.5),
        fg,
    );
    if resp.hovered() {
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}


// ── Folder rows ──────────────────────────────────────────────────────────────

struct FoldResp {
    select: bool,
    twisty: bool,
    bookmark: bool,
}

fn folder_row(
    ui: &mut egui::Ui,
    lib: &mut LibraryUi,
    tree_path: Option<&str>,
    depth: usize,
    is_leaf: bool,
    is_open: bool,
    on: bool,
    icon: &str,
    label: &str,
    count: usize,
    show_bm: bool,
    bm_on: bool,
) -> bool {
    folder_row_full(
        ui, lib, tree_path, depth, is_leaf, is_open, on, icon, label, count, bm_on, show_bm,
    )
    .select
}

fn folder_row_full(
    ui: &mut egui::Ui,
    lib: &mut LibraryUi,
    tree_path: Option<&str>,
    depth: usize,
    is_leaf: bool,
    is_open: bool,
    on: bool,
    icon: &str,
    label: &str,
    count: usize,
    bm_on: bool,
    show_bm: bool,
) -> FoldResp {
    let h = 22.0_f32;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), h),
        egui::Sense::click(),
    );
    if on {
        ui.painter().rect_filled(rect, 0.0, t::SURFACE_HEADER);
        ui.painter().rect_filled(
            egui::Rect::from_min_size(rect.left_top(), egui::vec2(2.0, h)),
            0.0,
            t::ACCENT,
        );
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, t::SURFACE_WELL);
    }

    let mut twisty = false;
    let mut bookmark = false;
    let pad = 6.0 + depth as f32 * 12.0;
    let mut x = rect.left() + pad;

    // Twisty
    if !is_leaf {
        let caret = if is_open {
            ph::CARET_DOWN
        } else {
            ph::CARET_RIGHT
        };
        let tr = egui::Rect::from_center_size(
            egui::pos2(x + 6.0, rect.center().y),
            egui::vec2(14.0, 14.0),
        );
        ui.painter().text(
            tr.center(),
            egui::Align2::CENTER_CENTER,
            caret,
            egui::FontId::proportional(10.0),
            t::TEXT_FAINT,
        );
        if resp.clicked() {
            // Prefer twisty if click is on the caret strip
            if let Some(p) = ui.ctx().pointer_interact_pos() {
                if tr.expand(4.0).contains(p) {
                    twisty = true;
                }
            }
        }
        x += 14.0;
    } else {
        x += 4.0;
    }

    ui.painter().text(
        egui::pos2(x + 8.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(12.0),
        if on { t::TEXT } else { t::TEXT_DIM },
    );
    x += 18.0;
    ui.painter().text(
        egui::pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(11.5),
        if on { t::TEXT } else { t::TEXT_SECONDARY },
    );

    // Count + optional bookmark on the right
    let mut right = rect.right() - 8.0;
    if show_bm {
        let br = egui::Rect::from_center_size(
            egui::pos2(right - 6.0, rect.center().y),
            egui::vec2(14.0, 14.0),
        );
        ui.painter().text(
            br.center(),
            egui::Align2::CENTER_CENTER,
            if bm_on { "★" } else { "☆" },
            egui::FontId::proportional(11.0),
            if bm_on { t::ACCENT_LIGHT } else { t::TEXT_FAINT },
        );
        if resp.clicked() {
            if let Some(p) = ui.ctx().pointer_interact_pos() {
                if br.expand(3.0).contains(p) {
                    bookmark = true;
                }
            }
        }
        right -= 18.0;
    }
    ui.painter().text(
        egui::pos2(right, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        format!("{count}"),
        egui::FontId::monospace(10.0),
        t::TEXT_FAINT,
    );

    let select = resp.clicked() && !twisty && !bookmark;
    if resp.hovered() {
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
    }
    // Folder system context menu (right-click)
    resp.azra_context_menu(|ui| {
        folder_context_menu(ui, lib, tree_path);
    });
    FoldResp {
        select,
        twisty,
        bookmark,
    }
}

/// Right-click menu for tree folders / empty “All” / background.
fn folder_context_menu(ui: &mut egui::Ui, lib: &mut LibraryUi, tree_path: Option<&str>) {
    ui.set_min_width(200.0);
    let title = tree_path
        .map(|p| leaf_name(p).to_string())
        .unwrap_or_else(|| "Library".into());
    ui.label(
        egui::RichText::new(&title)
            .size(11.0)
            .color(t::TEXT_DIM)
            .strong(),
    );
    ui.separator();

    // New folder / section
    if ui
        .button(format!("{}  New folder…", ph::FOLDER_PLUS))
        .on_hover_text("Create a subfolder on disk under this location")
        .clicked()
    {
        lib.folder_new = Some((tree_path.map(|s| s.to_string()), "New Folder".into()));
        ui.close_menu();
    }

    if let Some(path) = tree_path {
        let fav = lib.bookmarks.contains(path);
        let fav_lbl = if fav {
            format!("{}  Remove from favourites", ph::STAR)
        } else {
            format!("{}  Add to favourites", ph::STAR)
        };
        if ui.button(fav_lbl).clicked() {
            lib.pending = Some(LibCtxAction::FolderToggleFavorite {
                tree_path: path.to_string(),
            });
            ui.close_menu();
        }

        if ui
            .button(format!("{}  Rename…", ph::PENCIL_SIMPLE))
            .clicked()
        {
            lib.folder_rename = Some((path.to_string(), leaf_name(path).to_string()));
            ui.close_menu();
        }

        if ui
            .button(format!("{}  Duplicate", ph::COPY))
            .on_hover_text("Copy this folder on disk")
            .clicked()
        {
            lib.pending = Some(LibCtxAction::FolderDuplicate {
                tree_path: path.to_string(),
            });
            ui.close_menu();
        }

        ui.separator();

        if ui
            .button(format!("{}  Find in Explorer", ph::FOLDER_OPEN))
            .clicked()
        {
            lib.pending = Some(LibCtxAction::FolderReveal {
                tree_path: path.to_string(),
            });
            ui.close_menu();
        }

        // Expand / collapse this branch
        let open = lib.open_folders.contains(path);
        let exp = if open {
            format!("{}  Collapse", ph::CARET_UP)
        } else {
            format!("{}  Expand", ph::CARET_DOWN)
        };
        if ui.button(exp).clicked() {
            if open {
                lib.open_folders.remove(path);
            } else {
                lib.open_folders.insert(path.to_string());
            }
            ui.close_menu();
        }

        if ui
            .button(format!("{}  Select / open", ph::ARROW_RIGHT))
            .clicked()
        {
            lib.sel = Some(path.to_string());
            lib.open_folders.insert(path.to_string());
            ui.close_menu();
        }

        ui.separator();
        let del = ui.add(
            egui::Button::new(format!("{}  Delete…", ph::TRASH))
                .fill(egui::Color32::from_rgba_unmultiplied(0x3a, 0x18, 0x18, 0x40)),
        );
        if del.clicked() {
            lib.folder_confirm_delete = Some(path.to_string());
            ui.close_menu();
        }
    } else {
        // Root / “All” context
        if ui
            .button(format!("{}  Show all items", ph::STACK))
            .clicked()
        {
            lib.sel = None;
            ui.close_menu();
        }
        if ui
            .button(format!("{}  Expand all categories", ph::CARET_DOUBLE_DOWN))
            .clicked()
        {
            for c in ContentCategory::all() {
                lib.open_folders.insert(c.label().to_string());
            }
            ui.close_menu();
        }
        if ui
            .button(format!("{}  Collapse all", ph::CARET_DOUBLE_UP))
            .clicked()
        {
            lib.open_folders.clear();
            ui.close_menu();
        }
    }
}

// ── Body ─────────────────────────────────────────────────────────────────────

fn draw_body(ui: &mut egui::Ui, lib: &mut LibraryUi, content: &mut LibraryState, body_h: f32) {
    let body = {
        let r = ui.available_rect_before_wrap();
        egui::Rect::from_min_size(r.min, egui::vec2(r.width(), body_h))
    };
    let _ = ui.allocate_exact_size(body.size(), egui::Sense::hover());

    let seam_w = t::SEAM_W;
    let col_w = display_col_w(lib, body.width());
    let items_w = (body.width() - col_w - seam_w).max(40.0);

    let col_rect = egui::Rect::from_min_size(body.min, egui::vec2(col_w, body_h));
    let seam_rect = egui::Rect::from_min_size(
        egui::pos2(col_rect.right(), body.top()),
        egui::vec2(seam_w, body_h),
    );
    let items_rect = egui::Rect::from_min_max(
        egui::pos2(seam_rect.right(), body.top()),
        body.right_bottom(),
    );

    // SUB-TIMERS (owner 2026-08-06: paint=1340.9ms with shown=2278, paint=58.5ms with shown=0).
    // Both panes ARE virtualised (`show_rows` over the visible range only) and the tree's
    // per-row `count` is precomputed, so reading the code does not explain the cost. Splitting
    // the three panes is the same move that turned `unaccounted=1443ms` into `setup=284.5ms`.
    // Prints only on a slow body, so it costs nothing in the steady state.
    let _t_col = std::time::Instant::now();
    draw_folder_col(ui, lib, content, col_rect);
    let col_ms = _t_col.elapsed().as_secs_f32() * 1000.0;

    let _t_seam = std::time::Instant::now();
    draw_col_seam(ui, lib, seam_rect, body.width());
    let seam_ms = _t_seam.elapsed().as_secs_f32() * 1000.0;

    let _t_items = std::time::Instant::now();
    draw_items(ui, lib, content, items_rect, items_w);
    let items_ms = _t_items.elapsed().as_secs_f32() * 1000.0;

    // CONSOLE_AUDIT B4: same 8 ms / AZRA_PERF_SPAM gate as [LIB] (was 1 ms → every frame).
    let body_ms = col_ms + seam_ms + items_ms;
    let lib_gate = if crate::perf_spam_on() { 1.0 } else { 8.0 };
    if body_ms > lib_gate {
        eprintln!(
            "[LIB-BODY] folder_col={col_ms:.1}ms seam={seam_ms:.1}ms items={items_ms:.1}ms  \
             total={body_ms:.1}ms (tree_rows={} shown={}) | {}",
            lib.tree_rows.len(),
            lib.shown.len(),
            crate::profiling::perf_stamp()
        );
    }
}

fn draw_folder_col(ui: &mut egui::Ui, lib: &mut LibraryUi, content: &LibraryState, rect: egui::Rect) {
    ui.allocate_ui_at_rect(rect, |ui| {
        ui.set_min_size(rect.size());
        ui.set_max_size(rect.size());
        ui.set_clip_rect(rect);
        apply_lib_scroll_style(ui);

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let search_w = (ui.available_width() - 12.0).max(60.0);
            egui::Frame::none()
                .fill(t::SURFACE_FIELD)
                .rounding(5.0)
                .stroke(egui::Stroke::new(1.0, t::SEAM))
                .inner_margin(egui::Margin::symmetric(8, 3))
                .show(ui, |ui| {
                    ui.set_width(search_w - 16.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(ph::MAGNIFYING_GLASS)
                                .size(12.0)
                                .color(t::TEXT_FAINT),
                        );
                        let mut filter = lib.filter.clone();
                        let te = egui::TextEdit::singleline(&mut filter)
                            .desired_width(ui.available_width() - 22.0)
                            .hint_text("Filter")
                            .frame(false)
                            .font(egui::FontId::proportional(11.0));
                        if ui.add(te).changed() {
                            lib.filter = filter;
                        }
                        let n_cats = ContentCategory::all().len();
                        let active = lib.fav_only || lib.cats_on.len() < n_cats;
                        let funnel_col = if active { t::ACCENT_LIGHT } else { t::TEXT_FAINT };
                        let fr = ui.add(
                            egui::Label::new(
                                egui::RichText::new(ph::FUNNEL)
                                    .size(12.0)
                                    .color(funnel_col),
                            )
                            .sense(egui::Sense::click()),
                        );
                        if fr.clicked() {
                            lib.fav_only = !lib.fav_only;
                        }
                        fr.on_hover_text(if lib.fav_only {
                            "Favourites only (on) — click to clear"
                        } else {
                            "Favourites only — click to filter"
                        });
                    });
                });
        });
        ui.add_space(4.0);

        // tree_rows already ensured in draw_in_central; only rebuild if expand/bookmark just flipped.
        ensure_tree_rows(lib, content);
        let n_tree = lib.tree_rows.len();
        let row_h = 22.0_f32;
        let mut tree_action: Option<(usize, FoldResp)> = None;

        egui::ScrollArea::vertical()
            .id_salt("v2_lib_tree")
            .auto_shrink([false, false])
            .show_rows(ui, row_h, n_tree, |ui, row_range| {
                ui.set_width(ui.available_width());
                for ri in row_range {
                    let Some(row) = lib.tree_rows.get(ri).cloned() else {
                        continue;
                    };
                    let on = match &row.path {
                        None => lib.sel.is_none(),
                        Some(p) => lib.sel.as_deref() == Some(p.as_str()),
                    };
                    let resp = folder_row_full(
                        ui,
                        lib,
                        row.path.as_deref(),
                        row.depth,
                        row.is_leaf,
                        row.is_open,
                        on,
                        row.icon,
                        &row.label,
                        row.count,
                        row.bookmarked,
                        row.show_bookmark,
                    );
                    if resp.select || resp.twisty || resp.bookmark {
                        tree_action = Some((ri, resp));
                    }
                }
            });

        // Apply tree interaction after the borrow of tree_rows ends.
        if let Some((ri, resp)) = tree_action {
            let path = lib.tree_rows.get(ri).and_then(|r| r.path.clone());
            if resp.twisty {
                if let Some(ref p) = path {
                    if lib.open_folders.contains(p) {
                        lib.open_folders.remove(p);
                    } else {
                        lib.open_folders.insert(p.clone());
                    }
                    lib.tree_fp = TreeCacheFp::default(); // rebuild flatten
                }
            } else if resp.bookmark {
                if let Some(ref p) = path {
                    if lib.bookmarks.contains(p) {
                        lib.bookmarks.remove(p);
                    } else {
                        lib.bookmarks.insert(p.clone());
                    }
                    lib.tree_fp = TreeCacheFp::default();
                }
            } else if resp.select {
                match path {
                    None => lib.sel = None,
                    Some(p) => {
                        lib.sel = Some(p.clone());
                        // Opening a folder with kids expands it.
                        if lib
                            .folder_children
                            .get(&p)
                            .map(|k| !k.is_empty())
                            .unwrap_or(false)
                        {
                            lib.open_folders.insert(p);
                        }
                        lib.tree_fp = TreeCacheFp::default();
                    }
                }
            }
        }

        // Empty-area right-click (folder system root) — only when tree is short.
        if n_tree < 8 {
            let fill = ui.available_height().max(8.0);
            if fill > 4.0 {
                let (_empty, empty_r) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), fill),
                    egui::Sense::click(),
                );
                empty_r.azra_context_menu(|ui| {
                    folder_context_menu(ui, lib, None);
                });
            }
        }
    });
}

/// Rebuild the flat open-tree list when content / expansion / bookmarks change.
fn ensure_tree_rows(lib: &mut LibraryUi, content: &LibraryState) {
    let mut open: Vec<String> = lib.open_folders.iter().cloned().collect();
    open.sort();
    let mut bookmarks: Vec<String> = lib.bookmarks.iter().cloned().collect();
    bookmarks.sort();
    let fp = TreeCacheFp {
        content: content_fp(content),
        open,
        bookmarks,
    };
    if lib.tree_fp == fp && !lib.tree_rows.is_empty() {
        return;
    }

    let mut rows: Vec<TreeRow> = Vec::with_capacity(64);
    // Synthetic “All”
    rows.push(TreeRow {
        path: None,
        depth: 0,
        is_leaf: true,
        is_open: false,
        icon: ph::STACK,
        label: "All".into(),
        count: content.items.len(),
        bookmarked: false,
        show_bookmark: false,
    });

    for &cat in ContentCategory::all() {
        push_tree_branch(lib, &mut rows, cat.label(), 0);
    }

    lib.tree_rows = rows;
    lib.tree_fp = fp;
}

fn push_tree_branch(lib: &LibraryUi, rows: &mut Vec<TreeRow>, path: &str, depth: usize) {
    let kids = lib.folder_children.get(path);
    let is_leaf = kids.map(|k| k.is_empty()).unwrap_or(true);
    let is_open = lib.open_folders.contains(path);
    let n = lib.folder_counts.get(path).copied().unwrap_or(0);
    let icon = if depth == 0 {
        ContentCategory::all()
            .iter()
            .find(|c| c.label() == path)
            .map(|c| cat_icon(*c))
            .unwrap_or(ph::FOLDER)
    } else {
        ph::FOLDER
    };
    rows.push(TreeRow {
        path: Some(path.to_string()),
        depth,
        is_leaf,
        is_open,
        icon,
        label: leaf_name(path).to_string(),
        count: n,
        bookmarked: lib.bookmarks.contains(path),
        show_bookmark: !path.is_empty(),
    });
    if !is_leaf && is_open {
        if let Some(kids) = kids {
            for k in kids {
                push_tree_branch(lib, rows, k, depth + 1);
            }
        }
    }
}

/// The 1px divider between the folder column and the item grid.
///
/// INSTRUMENTED 2026-08-06. The owner's `[LIB-BODY]` line reports this consistently at
/// 8-38ms while `folder_col` and `items` sit at 0.0-0.1ms — with `shown=9`. A rect fill
/// and a drag hit-test cannot cost that, so the time is either (a) inside one specific
/// egui call here, or (b) a stall from elsewhere that this narrow window happens to
/// capture. Guessing between those is what the per-pane split already refuted once, so
/// this breaks the function into its four egui operations instead.
///
/// Prints only when the total exceeds the same 8ms gate as `[LIB-BODY]`, so it is free
/// in the steady state. REMOVE once the culprit is pinned.
fn draw_col_seam(ui: &mut egui::Ui, lib: &mut LibraryUi, rect: egui::Rect, body_w: f32) {
    let t0 = std::time::Instant::now();
    let mut t_alloc_ui = 0.0f32;
    let mut t_alloc_size = 0.0f32;
    let mut t_fill = 0.0f32;
    let mut t_interact = 0.0f32;
    ui.allocate_ui_at_rect(rect, |ui| {
        t_alloc_ui = t0.elapsed().as_secs_f32() * 1000.0;
        let t1 = std::time::Instant::now();
        ui.set_min_size(rect.size());
        ui.set_max_size(rect.size());
        ui.set_clip_rect(rect);
        let (r, resp) = ui.allocate_exact_size(rect.size(), egui::Sense::drag());
        t_alloc_size = t1.elapsed().as_secs_f32() * 1000.0;

        let t2 = std::time::Instant::now();
        ui.painter().rect_filled(r, 0.0, t::SEAM);
        t_fill = t2.elapsed().as_secs_f32() * 1000.0;

        let t3 = std::time::Instant::now();
        if resp.hovered() || resp.dragged() {
            ui.painter().rect_filled(r, 0.0, t::BORDER_HOVER);
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeHorizontal);
        }
        if resp.dragged() {
            let max_for_body = (body_w - t::SEAM_W - 80.0).clamp(LIB_COL_MIN, LIB_COL_MAX);
            lib.col_w = (lib.col_w + resp.drag_delta().x).clamp(LIB_COL_MIN, max_for_body);
        }
        t_interact = t3.elapsed().as_secs_f32() * 1000.0;
    });
    let total = t0.elapsed().as_secs_f32() * 1000.0;
    let gate = if crate::perf_spam_on() { 1.0 } else { 8.0 };
    if total > gate {
        // `closure_exit` = time inside allocate_ui_at_rect AFTER the closure returned,
        // i.e. egui's own end-of-child-Ui work. If the cost lands there rather than in
        // any of the four operations, the culprit is egui layout/tessellation for this
        // child Ui, not anything this function asked for.
        let inner = t_alloc_size + t_fill + t_interact;
        eprintln!(
            "[LIB-SEAM] total={total:.1}ms | enter_alloc_ui={t_alloc_ui:.1} \
             alloc_size={t_alloc_size:.1} fill={t_fill:.1} interact={t_interact:.1} \
             | closure_exit={:.1}",
            total - t_alloc_ui - inner
        );
    }
}

fn draw_items(
    ui: &mut egui::Ui,
    lib: &mut LibraryUi,
    content: &mut LibraryState,
    rect: egui::Rect,
    _items_w: f32,
) {
    ui.allocate_ui_at_rect(rect, |ui| {
        ui.set_min_size(rect.size());
        ui.set_max_size(rect.size());
        ui.set_clip_rect(rect);
        apply_lib_scroll_style(ui);

        // Ctrl/Cmd+scroll → zoom scale; consume wheel so ScrollArea does not pan.
        if ui.rect_contains_pointer(rect) {
            let ctrl = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
            if ctrl {
                let scroll_y: f32 = ui.input_mut(|i| {
                    let mut total = 0.0_f32;
                    i.events.retain(|e| {
                        if let egui::Event::MouseWheel { delta, .. } = e {
                            total += delta.y;
                            false
                        } else {
                            true
                        }
                    });
                    total
                });
                let zoom = ui.input(|i| i.zoom_delta());
                if (zoom - 1.0).abs() > 0.001 {
                    lib.scale = (lib.scale * zoom).clamp(0.5, 3.0);
                } else if scroll_y.abs() > 0.0 {
                    lib.scale = (lib.scale + scroll_y * 0.1).clamp(0.5, 3.0);
                }
            }
        }

        // Filter may have changed in the folder column this frame — refresh if needed.
        ensure_shown(lib, content);
        let thumb = (80.0 * lib.scale).round().clamp(40.0, 200.0);
        let n_shown = lib.shown.len();

        if n_shown == 0 {
            egui::ScrollArea::vertical()
                .id_salt("v2_lib_items")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add_space(14.0);
                    ui.add_space(8.0);
                    let msg = if content.items.is_empty() {
                        if content.scanned {
                            "No assets found under assets/"
                        } else {
                            "Scanning assets…"
                        }
                    } else {
                        "No items in this folder"
                    };
                    ui.label(
                        egui::RichText::new(msg)
                            .size(11.5)
                            .italics()
                            .color(t::TEXT_FAINT),
                    );
                });
            return;
        }

        // Virtualized rows: only allocate widgets for the scroll viewport (+ egui overscan).
        // Full-library grids were allocating every tile every frame (idle tax).
        let (row_h, cols, gap, is_list) = if lib.view == LibView::List {
            let row_h = (t::ROW_HEIGHT * lib.scale).round().clamp(22.0, 40.0);
            let col_w = (220.0 * lib.scale).round().clamp(160.0, 320.0);
            let gap = 4.0;
            let avail = (ui.available_width() - 16.0).max(col_w);
            let cols = ((avail + gap) / (col_w + gap)).floor().max(1.0) as usize;
            (row_h, cols, gap, true)
        } else {
            let gap = 8.0;
            let name_h = 22.0;
            let row_h = thumb + name_h;
            let avail = (ui.available_width() - 16.0).max(thumb);
            let cols = ((avail + gap) / (thumb + gap)).floor().max(1.0) as usize;
            (row_h, cols, gap, false)
        };
        let n_rows = (n_shown + cols - 1) / cols;
        // Capture list col width for the row closure.
        let list_col_w = if is_list {
            (220.0 * lib.scale).round().clamp(160.0, 320.0)
        } else {
            0.0
        };

        let out = egui::ScrollArea::vertical()
            .id_salt("v2_lib_items")
            .auto_shrink([false, false])
            .show_rows(ui, row_h, n_rows, |ui, row_range| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
                for row in row_range {
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.spacing_mut().item_spacing.x = gap;
                        for c in 0..cols {
                            let i = row * cols + c;
                            if i >= n_shown {
                                break;
                            }
                            let idx = lib.shown[i];
                            lib.visible_for_thumbs.push(idx);
                            if is_list {
                                list_row_sized(ui, content, idx, lib, list_col_w, row_h);
                            } else {
                                grid_tile(ui, content, idx, thumb, lib);
                            }
                        }
                    });
                }
            });
        // Keep a small overscan of thumb indices if scroll is mid-row (range is exact).
        let _ = out;
    });
}

fn draw_info(ui: &mut egui::Ui, lib: &mut LibraryUi, content: &LibraryState, info_h: f32) {
    let bar = {
        let r = ui.available_rect_before_wrap();
        egui::Rect::from_min_size(r.min, egui::vec2(r.width(), info_h))
    };
    let _ = ui.allocate_rect(bar, egui::Sense::hover());
    ui.painter().hline(
        bar.x_range(),
        bar.top() + 0.5,
        egui::Stroke::new(1.0, t::SEAM),
    );

    ensure_shown(lib, content);
    let n_shown = lib.shown.len();
    let n_all = content.items.len();
    let pct = (lib.scale * 100.0).round() as i32;
    // Forever index / first scan already stored this — never walk FS on the paint path.
    let root = if content.asset_root_display.is_empty() {
        "(assets)"
    } else {
        content.asset_root_display.as_str()
    };

    ui.allocate_ui_at_rect(bar, |ui| {
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add_space(10.0);
            let icon = if lib.sel.is_some() { ph::FOLDER } else { ph::STACK };
            ui.label(egui::RichText::new(icon).size(12.0).color(t::TEXT_LABEL));
            let crumb = lib
                .sel
                .as_deref()
                .unwrap_or("All")
                .replace('/', "  /  ");
            ui.label(
                egui::RichText::new(crumb)
                    .size(10.5)
                    .color(t::TEXT_DIM),
            );
            if !lib.filter.is_empty() {
                ui.label(
                    egui::RichText::new(format!("· filter « {} »", lib.filter))
                        .size(10.5)
                        .color(t::TEXT_FAINT),
                );
            }
            ui.label(
                egui::RichText::new(format!("· {root}"))
                    .size(9.5)
                    .color(t::TEXT_FAINT),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(format!("{pct}%"))
                        .size(10.5)
                        .color(t::TEXT_FAINT)
                        .family(egui::FontFamily::Monospace),
                );
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(format!("{n_shown} / {n_all} items"))
                        .size(10.5)
                        .color(t::TEXT_FAINT)
                        .family(egui::FontFamily::Monospace),
                );
            });
        });
    });
}

// ── Tiles (real thumbnails, no placeholder swatches) ─────────────────────────

fn item_context_menu(ui: &mut egui::Ui, lib: &mut LibraryUi, content: &LibraryState, idx: usize) {
    let Some(item) = content.items.get(idx) else { return; };
    let name = item.name.clone();
    let cat = item.category;
    let fav = item.favorite;
    let applies_to_object = matches!(
        cat,
        ContentCategory::Materials
            | ContentCategory::Textures
            | ContentCategory::MatCaps
            | ContentCategory::Bodies
    );

    ui.set_min_width(200.0);
    ui.label(
        egui::RichText::new(&name)
            .size(11.0)
            .color(t::TEXT_DIM)
            .strong(),
    );
    ui.separator();

    if ui
        .add(egui::Button::new(format!("{}  Find in Explorer", ph::FOLDER_OPEN)))
        .clicked()
    {
        lib.pending = Some(LibCtxAction::RevealInExplorer { item_idx: idx });
        ui.close_menu();
    }

    let fav_label = if fav {
        format!("{}  Unmark favourite", ph::STAR)
    } else {
        format!("{}  Mark as favourite", ph::STAR)
    };
    if ui.add(egui::Button::new(fav_label)).clicked() {
        lib.pending = Some(LibCtxAction::ToggleFavorite { item_idx: idx });
        ui.close_menu();
    }

    if ui
        .add(egui::Button::new(format!("{}  Rename…", ph::PENCIL_SIMPLE)))
        .clicked()
    {
        lib.rename = Some((idx, name.clone()));
        ui.close_menu();
    }

    if ui
        .add(egui::Button::new(format!("{}  Duplicate", ph::COPY)))
        .clicked()
    {
        lib.pending = Some(LibCtxAction::Duplicate { item_idx: idx });
        ui.close_menu();
    }

    if ui
        .add(egui::Button::new(format!("{}  Create group", ph::FOLDER_PLUS)))
        .clicked()
    {
        lib.pending = Some(LibCtxAction::CreateGroup { item_idx: idx });
        ui.close_menu();
    }

    ui.separator();

    if applies_to_object {
        // How many assignable objects are selected (passed from app via can_apply + count).
        let n_sel = lib.apply_target_count.max(if lib.can_apply { 1 } else { 0 });
        let apply_label = match cat {
            ContentCategory::MatCaps => format!("{}  Apply matcap to selection", ph::CIRCLE_HALF),
            ContentCategory::Bodies => format!("{}  Insert into scene", ph::CUBE),
            // Materials / textures: multi-select → "Add to N selected objects".
            ContentCategory::Materials | ContentCategory::Textures => {
                if n_sel > 1 {
                    format!("{}  Add to {} selected objects", ph::PLUS_CIRCLE, n_sel)
                } else {
                    format!("{}  Add to selected poly / object", ph::PLUS_CIRCLE)
                }
            }
            _ => {
                if n_sel > 1 {
                    format!("{}  Add to {} selected", ph::PLUS_CIRCLE, n_sel)
                } else {
                    format!("{}  Add to selection", ph::PLUS_CIRCLE)
                }
            }
        };
        // Bodies insert without a selection; matcaps are global; materials need a target.
        let needs_sel = matches!(cat, ContentCategory::Materials | ContentCategory::Textures);
        let enabled = !needs_sel || lib.can_apply;
        let mut resp = ui.add_enabled(enabled, egui::Button::new(apply_label));
        if !enabled {
            resp = resp.on_hover_text("Select a poly mesh or SDF object first");
        } else if needs_sel {
            resp = resp.on_hover_text(if n_sel > 1 {
                "Assign this material to EVERY selected poly / SDF object (shared material, one undo)"
            } else {
                "Assign this material to the selected poly body (all faces, or selected faces in Face mode) or SDF object"
            });
        }
        if enabled && resp.clicked() {
            lib.pending = Some(LibCtxAction::ApplyToSelection { item_idx: idx });
            ui.close_menu();
        }
        // MatCaps only: remember this as the default for next launch + MatCap mode.
        if cat == ContentCategory::MatCaps {
            if ui
                .add(egui::Button::new(format!(
                    "{}  Set as default matcap",
                    ph::CHECK_CIRCLE
                )))
                .on_hover_text(
                    "Use this matcap at startup and whenever MatCap shading is the default.\n\
                     Saved to %LOCALAPPDATA%/Azra/default_matcap.txt",
                )
                .clicked()
            {
                lib.pending = Some(LibCtxAction::SetDefaultMatcap { item_idx: idx });
                ui.close_menu();
            }
        }
    } else if cat == ContentCategory::HDRIs {
        if ui
            .add(egui::Button::new(format!("{}  Apply environment", ph::SUN)))
            .clicked()
        {
            lib.pending = Some(LibCtxAction::ApplyToSelection { item_idx: idx });
            ui.close_menu();
        }
    }

    ui.separator();
    let del = ui.add(
        egui::Button::new(format!("{}  Delete…", ph::TRASH))
            .fill(egui::Color32::from_rgba_unmultiplied(0x3a, 0x18, 0x18, 0x40)),
    );
    if del.clicked() {
        lib.confirm_delete = Some(idx);
        ui.close_menu();
    }
}

fn wire_item_response(
    ui: &mut egui::Ui,
    resp: &egui::Response,
    content: &LibraryState,
    idx: usize,
    lib: &mut LibraryUi,
) {
    let Some(item) = content.items.get(idx) else { return; };
    if resp.clicked() {
        lib.selected = Some(idx);
    }
    // Right-click opens the menu (and selects).
    if resp.secondary_clicked() {
        lib.selected = Some(idx);
    }
    resp.azra_context_menu(|ui| {
        item_context_menu(ui, lib, content, idx);
    });

    let folder = item_tree_path(item);
    if resp.drag_started() || resp.dragged() {
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Grabbing);
        lib.drag = Some((idx, item.category, item.name.clone(), folder));
        egui::DragAndDrop::set_payload(
            ui.ctx(),
            LibraryDragPayload {
                item_idx: idx,
                category: item.category,
            },
        );
    } else if resp.hovered() {
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Grab);
        resp.clone().on_hover_text(format!(
            "{}  ·  {}\nRight-click for options",
            item.name,
            item.folder_path.display()
        ));
    }
}

fn grid_tile(
    ui: &mut egui::Ui,
    content: &LibraryState,
    idx: usize,
    thumb: f32,
    lib: &mut LibraryUi,
) {
    let Some(item) = content.items.get(idx) else { return; };
    let name_h = 22.0;
    let w = thumb;
    let h = thumb + name_h;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click_and_drag());
    let selected = lib.selected == Some(idx);
    let round = egui::Rounding::same(6);
    let border = if selected {
        t::ACCENT
    } else if resp.hovered() {
        t::BORDER_HOVER
    } else {
        t::SEAM
    };
    ui.painter().rect_filled(rect, round, t::SURFACE_CARD);
    ui.painter().rect_stroke(
        rect,
        round,
        egui::Stroke::new(if selected { 1.5 } else { 1.0 }, border), egui::StrokeKind::Middle,
    );

    let th = egui::Rect::from_min_size(rect.min, egui::vec2(w, thumb));
    let th_round = egui::Rounding {
        nw: 6, ne: 6, sw: 0, se: 0,
    };
    if let Some(ref handle) = item.thumb_handle {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter().image(handle.id(), th, uv, egui::Color32::WHITE);
    } else {
        ui.painter().rect_filled(th, th_round, t::SURFACE_FIELD);
        let label = if item.thumb_attempted { "…" } else { " " };
        ui.painter().text(
            th.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(14.0),
            t::TEXT_FAINT,
        );
    }

    if !item.texture_role.is_empty() {
        let g = ui.fonts(|f| {
            f.layout_no_wrap(
                item.texture_role.clone(),
                egui::FontId::monospace(8.5),
                t::TEXT_SECONDARY,
            )
        });
        let br = egui::Rect::from_min_size(
            th.left_top() + egui::vec2(4.0, 4.0),
            g.size() + egui::vec2(8.0, 4.0),
        );
        ui.painter()
            .rect_filled(br, 3.0, egui::Color32::from_black_alpha(0x99));
        ui.painter().text(
            br.center(),
            egui::Align2::CENTER_CENTER,
            &item.texture_role,
            egui::FontId::monospace(8.5),
            t::TEXT_SECONDARY,
        );
    }
    if item.favorite {
        ui.painter().text(
            th.right_top() + egui::vec2(-10.0, 10.0),
            egui::Align2::CENTER_CENTER,
            "★",
            egui::FontId::proportional(11.0),
            t::ACCENT_LIGHT,
        );
    }

    let nm = egui::Rect::from_min_max(
        egui::pos2(rect.left(), th.bottom()),
        rect.right_bottom(),
    );
    ui.painter().rect_filled(nm, 0.0, t::SURFACE_CARD);
    ui.painter().text(
        nm.center(),
        egui::Align2::CENTER_CENTER,
        &item.name,
        egui::FontId::proportional(10.5),
        t::TEXT_SECONDARY,
    );

    wire_item_response(ui, &resp, content, idx, lib);
}

fn list_row_sized(
    ui: &mut egui::Ui,
    content: &LibraryState,
    idx: usize,
    lib: &mut LibraryUi,
    col_w: f32,
    row_h: f32,
) {
    let Some(item) = content.items.get(idx) else {
        return;
    };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(col_w, row_h), egui::Sense::click_and_drag());
    let selected = lib.selected == Some(idx);
    if selected {
        ui.painter().rect_filled(rect, 4.0, t::accent_tint(40));
        ui.painter()
            .rect_stroke(rect, 4.0, egui::Stroke::new(1.0, t::ACCENT), egui::StrokeKind::Middle);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, t::SURFACE_HEADER);
    }
    let icon = (row_h - 6.0).clamp(14.0, 28.0);
    let sw = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 4.0 + icon * 0.5, rect.center().y),
        egui::vec2(icon, icon),
    );
    if let Some(ref handle) = item.thumb_handle {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter().image(handle.id(), sw, uv, egui::Color32::WHITE);
    } else {
        ui.painter().rect_filled(sw, 3.0, t::SURFACE_FIELD);
    }
    let name_x = rect.left() + 8.0 + icon;
    let name_max_w = (rect.right() - 16.0 - name_x).max(20.0);
    let font = egui::FontId::proportional((11.0 * lib.scale).clamp(10.0, 14.0));
    // Clip long names with at most 2 layouts (no per-char loop).
    let galley = ui.fonts(|f| f.layout_no_wrap(item.name.clone(), font.clone(), t::TEXT_SECONDARY));
    let draw_name = if galley.size().x > name_max_w {
        let approx = ((name_max_w / galley.size().x) * item.name.len() as f32) as usize;
        let take = approx.saturating_sub(1).max(1).min(item.name.len());
        // Prefer char boundary.
        let mut end = take;
        while end > 0 && !item.name.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &item.name[..end])
    } else {
        item.name.clone()
    };
    ui.painter().text(
        egui::pos2(name_x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        draw_name,
        font,
        t::TEXT_SECONDARY,
    );
    if item.favorite {
        ui.painter().text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            egui::Align2::CENTER_CENTER,
            "★",
            egui::FontId::proportional(11.0),
            t::ACCENT_LIGHT,
        );
    }
    wire_item_response(ui, &resp, content, idx, lib);
}

/// Rename + delete confirmation dialogs for the library.
fn draw_lib_dialogs(ctx: &egui::Context, lib: &mut LibraryUi, content: &LibraryState) {
    // ── Rename ──
    let mut rename_close = false;
    let mut rename_confirm: Option<(usize, String)> = None;
    if let Some((idx, draft)) = lib.rename.as_mut() {
        let idx = *idx;
        let title = content
            .items
            .get(idx)
            .map(|i| format!("Rename “{}”", i.name))
            .unwrap_or_else(|| "Rename".into());
        let mut open = true;
        let mut confirmed = false;
        let mut cancelled = false;
        egui::Window::new(title)
            .id(egui::Id::new("v2_lib_rename"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(280.0);
                ui.add_space(4.0);
                let te = ui.add(
                    egui::TextEdit::singleline(draft)
                        .desired_width(260.0)
                        .hint_text("New name"),
                );
                te.request_focus();
                if te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    confirmed = true;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancelled = true;
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(!draft.trim().is_empty(), egui::Button::new("Rename"))
                            .clicked()
                        {
                            confirmed = true;
                        }
                    });
                });
            });
        if !open || cancelled {
            rename_close = true;
        } else if confirmed {
            let new_name = draft.trim().to_string();
            if !new_name.is_empty() {
                rename_confirm = Some((idx, new_name));
            }
            rename_close = true;
        }
    }
    if rename_close {
        lib.rename = None;
    }
    if let Some((item_idx, new_name)) = rename_confirm {
        lib.pending = Some(LibCtxAction::Rename { item_idx, new_name });
    }

    // ── Delete confirm ──
    let mut del_close = false;
    let mut del_confirm: Option<usize> = None;
    if let Some(idx) = lib.confirm_delete {
        let name = content
            .items
            .get(idx)
            .map(|i| i.name.as_str())
            .unwrap_or("item");
        let mut open = true;
        let mut do_delete = false;
        let mut cancel = false;
        egui::Window::new("Delete library item")
            .id(egui::Id::new("v2_lib_delete"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(300.0);
                ui.label(format!(
                    "Permanently delete “{}” from disk?\nThis cannot be undone.",
                    name
                ));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let r = ui.add(
                            egui::Button::new("Delete")
                                .fill(egui::Color32::from_rgb(0x8a, 0x28, 0x28)),
                        );
                        if r.clicked() {
                            do_delete = true;
                        }
                    });
                });
            });
        if !open || cancel {
            del_close = true;
        } else if do_delete {
            del_confirm = Some(idx);
            del_close = true;
        }
    }
    if del_close {
        lib.confirm_delete = None;
    }
    if let Some(item_idx) = del_confirm {
        lib.pending = Some(LibCtxAction::Delete { item_idx });
    }

    // ── Folder rename ──
    let mut fr_close = false;
    let mut fr_confirm: Option<(String, String)> = None;
    if let Some((path, draft)) = lib.folder_rename.as_mut() {
        let path_c = path.clone();
        let mut open = true;
        let mut confirmed = false;
        let mut cancelled = false;
        egui::Window::new(format!("Rename folder “{}”", leaf_name(&path_c)))
            .id(egui::Id::new("v2_lib_folder_rename"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(280.0);
                let te = ui.add(
                    egui::TextEdit::singleline(draft)
                        .desired_width(260.0)
                        .hint_text("Folder name"),
                );
                te.request_focus();
                if te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    confirmed = true;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancelled = true;
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(!draft.trim().is_empty(), egui::Button::new("Rename"))
                            .clicked()
                        {
                            confirmed = true;
                        }
                    });
                });
            });
        if !open || cancelled {
            fr_close = true;
        } else if confirmed {
            let name = draft.trim().to_string();
            if !name.is_empty() {
                fr_confirm = Some((path_c, name));
            }
            fr_close = true;
        }
    }
    if fr_close {
        lib.folder_rename = None;
    }
    if let Some((tree_path, new_name)) = fr_confirm {
        lib.pending = Some(LibCtxAction::FolderRename { tree_path, new_name });
    }

    // ── Folder delete ──
    let mut fd_close = false;
    let mut fd_confirm: Option<String> = None;
    if let Some(ref path) = lib.folder_confirm_delete {
        let name = leaf_name(path).to_string();
        let mut open = true;
        let mut do_delete = false;
        let mut cancel = false;
        egui::Window::new("Delete folder")
            .id(egui::Id::new("v2_lib_folder_delete"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(300.0);
                ui.label(format!(
                    "Permanently delete folder “{}” and all contents from disk?\nThis cannot be undone.",
                    name
                ));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new("Delete")
                                    .fill(egui::Color32::from_rgb(0x8a, 0x28, 0x28)),
                            )
                            .clicked()
                        {
                            do_delete = true;
                        }
                    });
                });
            });
        if !open || cancel {
            fd_close = true;
        } else if do_delete {
            fd_confirm = Some(path.clone());
            fd_close = true;
        }
    }
    if fd_close {
        lib.folder_confirm_delete = None;
    }
    if let Some(tree_path) = fd_confirm {
        lib.pending = Some(LibCtxAction::FolderDelete { tree_path });
    }

    // ── New folder ──
    let mut fn_close = false;
    let mut fn_confirm: Option<(Option<String>, String)> = None;
    if let Some((parent, draft)) = lib.folder_new.as_mut() {
        let parent_c = parent.clone();
        let mut open = true;
        let mut confirmed = false;
        let mut cancelled = false;
        let title = parent_c
            .as_ref()
            .map(|p| format!("New folder in “{}”", leaf_name(p)))
            .unwrap_or_else(|| "New folder".into());
        egui::Window::new(title)
            .id(egui::Id::new("v2_lib_folder_new"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(280.0);
                let te = ui.add(
                    egui::TextEdit::singleline(draft)
                        .desired_width(260.0)
                        .hint_text("Folder name"),
                );
                te.request_focus();
                if te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    confirmed = true;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancelled = true;
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(!draft.trim().is_empty(), egui::Button::new("Create"))
                            .clicked()
                        {
                            confirmed = true;
                        }
                    });
                });
            });
        if !open || cancelled {
            fn_close = true;
        } else if confirmed {
            let name = draft.trim().to_string();
            if !name.is_empty() {
                fn_confirm = Some((parent_c, name));
            }
            fn_close = true;
        }
    }
    if fn_close {
        lib.folder_new = None;
    }
    if let Some((parent_tree, name)) = fn_confirm {
        lib.pending = Some(LibCtxAction::FolderNew { parent_tree, name });
    }
}

// ── Data helpers (real LibraryState) + draw caches ───────────────────────────

fn content_fp(content: &LibraryState) -> (usize, u32) {
    (content.items.len(), content.next_item_id)
}

fn shown_fp_now(lib: &LibraryUi, content: &LibraryState) -> ShownCacheFp {
    let mut cats: Vec<String> = lib.cats_on.iter().cloned().collect();
    cats.sort();
    ShownCacheFp {
        content: content_fp(content),
        filter: lib.filter.clone(),
        sel: lib.sel.clone(),
        fav_only: lib.fav_only,
        cats,
        fav_n: lib.favorites.len(),
    }
}

/// Rebuild folder children + under-counts once per content change (not per tree row).
fn ensure_folder_index(lib: &mut LibraryUi, content: &LibraryState) {
    let fp = content_fp(content);
    if lib.folder_fp == fp {
        return;
    }

    let mut all_paths: HashSet<String> = HashSet::new();
    for c in ContentCategory::all() {
        all_paths.insert(c.label().to_string());
    }
    let mut counts: HashMap<String, usize> = HashMap::new();
    for item in &content.items {
        let path = item_tree_path(item);
        let parts: Vec<&str> = path.split('/').collect();
        for k in 1..=parts.len() {
            let p = parts[..k].join("/");
            all_paths.insert(p.clone());
            *counts.entry(p).or_insert(0) += 1;
        }
    }
    // Ensure category roots exist in counts even when empty.
    for c in ContentCategory::all() {
        counts.entry(c.label().to_string()).or_insert(0);
    }

    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    let mut sorted: Vec<String> = all_paths.into_iter().collect();
    sorted.sort();
    for p in &sorted {
        if let Some((parent, _)) = p.rsplit_once('/') {
            children.entry(parent.to_string()).or_default().push(p.clone());
        }
    }
    for v in children.values_mut() {
        v.sort();
    }

    lib.folder_children = children;
    lib.folder_counts = counts;
    lib.folder_fp = fp;
}

/// Rebuild shown indices only when filter / folder / fav / cats / content change.
/// CONSOLE_AUDIT C1: amortize across frames (≤`MAX_LIB_SHOWN_MS_PER_FRAME`) so a content
/// fingerprint change never costs 200+ ms in one paint.
fn ensure_shown(lib: &mut LibraryUi, content: &LibraryState) {
    let fp = shown_fp_now(lib, content);
    if lib.shown_fp == fp && lib.shown_build_at.is_none() {
        return;
    }

    // New target (or first start): reset progressive scan.
    let restart = match &lib.shown_building_fp {
        Some(b) if b == &fp => false,
        _ => true,
    };
    if restart {
        if content.items.len() > 64 {
            eprintln!(
                "[LIB-CACHE] shown rebuild start items={} filter={:?} sel={:?} fav_only={} cats={}",
                content.items.len(),
                fp.filter,
                fp.sel,
                fp.fav_only,
                fp.cats.len()
            );
        }
        lib.shown.clear();
        lib.shown_build_at = Some(0);
        lib.shown_building_fp = Some(fp.clone());
        // Incomplete until scan finishes — clear completed fingerprint.
        lib.shown_fp = ShownCacheFp::default();
    }

    let Some(mut at) = lib.shown_build_at else {
        return;
    };
    let q = fp.filter.to_lowercase();
    let cats: HashSet<&str> = fp.cats.iter().map(|s| s.as_str()).collect();
    let t0 = std::time::Instant::now();
    let n = content.items.len();
    while at < n {
        if let Some(i) = content.items.get(at) {
            let path = item_tree_path(i);
            let root = path.split('/').next().unwrap_or("");
            let mut ok = cats.contains(root);
            if ok {
                if let Some(ref sel) = fp.sel {
                    if !(path == *sel || path.starts_with(&format!("{sel}/"))) {
                        ok = false;
                    }
                }
            }
            if ok && fp.fav_only && !i.favorite {
                ok = false;
            }
            if ok && !q.is_empty() && !i.name.to_lowercase().contains(&q) {
                ok = false;
            }
            if ok {
                lib.shown.push(at);
            }
        }
        at += 1;
        if t0.elapsed().as_secs_f32() * 1000.0 >= MAX_LIB_SHOWN_MS_PER_FRAME {
            break;
        }
    }
    let slice_ms = t0.elapsed().as_secs_f32() * 1000.0;
    if at >= n {
        lib.shown_fp = fp;
        lib.shown_build_at = None;
        lib.shown_building_fp = None;
        if slice_ms > 1.0 || n > 64 {
            eprintln!(
                "[LIB-CACHE] shown rebuild done n_shown={} items={} last_slice={:.1}ms",
                lib.shown.len(),
                n,
                slice_ms
            );
        }
    } else {
        lib.shown_build_at = Some(at);
        if slice_ms > 1.0 {
            eprintln!(
                "[LIB-CACHE] shown rebuild progress {}/{} slice={:.1}ms n_shown={}",
                at,
                n,
                slice_ms,
                lib.shown.len()
            );
        }
    }
}

/// Indices into `content.items` that match current folder / filter / fav / category chips.
/// Prefer cached `lib.shown` when still valid; otherwise compute without mutating.
pub fn shown_item_indices(lib: &LibraryUi, content: &LibraryState) -> Vec<usize> {
    if lib.shown_fp == shown_fp_now(lib, content) {
        return lib.shown.clone();
    }
    // Fallback (e.g. thumbs before first draw this frame): compute once.
    let q = lib.filter.to_lowercase();
    content
        .items
        .iter()
        .enumerate()
        .filter(|(_, i)| {
            let path = item_tree_path(i);
            let root = path.split('/').next().unwrap_or("");
            if !lib.cats_on.contains(root) {
                return false;
            }
            if let Some(ref sel) = lib.sel {
                if !(path == *sel || path.starts_with(&format!("{sel}/"))) {
                    return false;
                }
            }
            if lib.fav_only && !i.favorite {
                return false;
            }
            if !q.is_empty() && !i.name.to_lowercase().contains(&q) {
                return false;
            }
            true
        })
        .map(|(idx, _)| idx)
        .collect()
}

/// Visible viewport indices for thumbnail loading (subset of shown).
///
/// The library draw runs before thumbnail scheduling, so an empty set means that no tiles are
/// visible. Falling back to every shown item here makes a collapsed/empty pane scan the entire
/// library on every frame.
pub fn visible_thumb_indices(lib: &LibraryUi) -> &[usize] {
    &lib.visible_for_thumbs
}

#[cfg(test)]
mod o2_tests {
    use super::*;

    /// O2: zone-closed path must not need a populated `shown` cache (strip-only).
    #[test]
    fn o2_closed_library_strip_does_not_require_shown() {
        let lib = LibraryUi {
            open: false,
            ..LibraryUi::default()
        };
        assert!(!lib.open);
        assert!(visible_thumb_indices(&lib).is_empty());
    }

    #[test]
    fn empty_visible_tiles_do_not_scan_the_full_library() {
        let lib = LibraryUi {
            open: true,
            shown: (0..10_000).collect(),
            visible_for_thumbs: Vec::new(),
            ..LibraryUi::default()
        };

        assert!(visible_thumb_indices(&lib).is_empty());
    }

    /// FRAME_STALL_PLAN Gate 2: shown list rebuilds only when the fingerprint changes.
    #[test]
    fn shown_cache_skips_rebuild_when_fingerprint_stable() {
        let mut content = LibraryState::default();
        content.items.push(ContentItem {
            id: 1,
            name: "A".into(),
            category: ContentCategory::Materials,
            folder_path: std::path::PathBuf::new(),
            thumb_path: None,
            files: Vec::new(),
            thumb_handle: None,
            thumb_attempted: false,
            tags: Vec::new(),
            favorite: false,
            is_individual_file: false,
            parent_folder: String::new(),
            texture_role: String::new(),
        });
        content.next_item_id = 2;
        let mut lib = LibraryUi::default();
        ensure_shown(&mut lib, &content);
        let first_ptr = lib.shown.as_ptr();
        let first_len = lib.shown.len();
        assert_eq!(first_len, 1);
        // Stable filter/content → ensure_shown is a no-op (same Vec allocation).
        ensure_shown(&mut lib, &content);
        assert_eq!(lib.shown.as_ptr(), first_ptr, "cache hit must not reallocate shown");
        assert_eq!(lib.shown.len(), first_len);
        // Filter change must rebuild.
        lib.filter = "nope".into();
        ensure_shown(&mut lib, &content);
        assert!(lib.shown.is_empty(), "filter miss should empty shown");
    }

    /// Gate 2: folder tree flatten is cached; expand toggles force rebuild.
    #[test]
    fn tree_rows_cache_hits_until_expand() {
        let content = LibraryState::default();
        let mut lib = LibraryUi::default();
        ensure_folder_index(&mut lib, &content);
        ensure_tree_rows(&mut lib, &content);
        let n0 = lib.tree_rows.len();
        let ptr0 = lib.tree_rows.as_ptr();
        ensure_tree_rows(&mut lib, &content);
        assert_eq!(lib.tree_rows.as_ptr(), ptr0, "stable expand set must cache tree_rows");
        assert_eq!(lib.tree_rows.len(), n0);
        // Collapse a category → fingerprint changes → rebuild.
        if let Some(cat) = ContentCategory::all().first() {
            lib.open_folders.remove(cat.label());
            ensure_tree_rows(&mut lib, &content);
            // May be shorter when a root category is closed.
            assert!(lib.tree_rows.len() <= n0);
        }
    }

    /// Owner Gate 2 repro: paint a library with ~2278 items and a deep folder tree under
    /// a headless egui context, and print the same `[LIB-BODY]` three-way split the live
    /// app emits. Does not assert a budget (measurement only) — acceptance is still the
    /// live `[Overlays] library < 1 ms` line with real assets.
    #[test]
    fn gate2_lib_body_split_with_2278_items() {
        // ~130 tree rows: 5 categories × ~25 subfolders each + "All".
        let mut content = LibraryState::default();
        let cats = [
            ContentCategory::Materials,
            ContentCategory::HDRIs,
            ContentCategory::Textures,
            ContentCategory::MatCaps,
            ContentCategory::Bodies,
        ];
        let n_items = 2278usize;
        for i in 0..n_items {
            let cat = cats[i % cats.len()];
            let sub = format!("Set{:03}", i % 125); // 125 unique parent folders
            content.items.push(ContentItem {
                id: (i as u32) + 1,
                name: format!("Asset_{i:04}"),
                category: cat,
                folder_path: std::path::PathBuf::from(format!("/fake/{sub}")),
                thumb_path: None,
                files: Vec::new(),
                thumb_handle: None,
                thumb_attempted: false,
                tags: Vec::new(),
                favorite: false,
                is_individual_file: false,
                parent_folder: sub,
                texture_role: String::new(),
            });
        }
        content.next_item_id = (n_items as u32) + 1;

        let mut lib = LibraryUi::default();
        lib.open = true;
        lib.collapsed = false;
        lib.height = 280.0;
        lib.sel = None; // "All" → full shown set
        // Expand every category root so the tree is deep (owner tree_rows≈130).
        for c in ContentCategory::all() {
            lib.open_folders.insert(c.label().to_string());
            // Open a slice of subfolders so flatten grows.
            for k in 0..25 {
                lib.open_folders
                    .insert(format!("{}/Set{:03}", c.label(), k));
            }
        }

        // Warm caches once (matches post-scan first draw).
        ensure_folder_index(&mut lib, &content);
        ensure_shown(&mut lib, &content);
        ensure_tree_rows(&mut lib, &content);
        assert_eq!(lib.shown.len(), n_items, "All view must show every item");
        assert!(
            lib.tree_rows.len() > 30,
            "tree must be non-trivial (got {} rows)",
            lib.tree_rows.len()
        );

        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 800.0));
        let mut samples: Vec<(f32, f32, f32, f32)> = Vec::new();
        for frame in 0..8 {
            let mut raw = egui::RawInput::default();
            raw.screen_rect = Some(screen);
            // Simulate a tiny pointer nudge so idle-gating paths (if any) still paint.
            raw.events.push(egui::Event::PointerMoved(egui::pos2(
                200.0 + frame as f32,
                700.0,
            )));
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    // Force a large available rect so body height is real.
                    ui.set_min_size(screen.size());
                    let _ = draw_in_central(ui, &mut lib, &mut content, screen);
                });
            });
            // draw_body already eprintln's when >1ms; re-time ensure+paint for the sample.
            let t0 = std::time::Instant::now();
            ensure_folder_index(&mut lib, &content);
            ensure_shown(&mut lib, &content);
            ensure_tree_rows(&mut lib, &content);
            let ensure_ms = t0.elapsed().as_secs_f32() * 1000.0;
            let t1 = std::time::Instant::now();
            // Second full draw to measure pure paint after widgets already exist.
            let mut raw2 = egui::RawInput::default();
            raw2.screen_rect = Some(screen);
            let _ = ctx.run(raw2, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.set_min_size(screen.size());
                    let _ = draw_in_central(ui, &mut lib, &mut content, screen);
                });
            });
            let paint_ms = t1.elapsed().as_secs_f32() * 1000.0;
            samples.push((ensure_ms, paint_ms, lib.tree_rows.len() as f32, lib.shown.len() as f32));
            eprintln!(
                "[LIB-BENCH] frame={frame} ensure={ensure_ms:.2}ms paint={paint_ms:.2}ms \
                 tree_rows={} shown={}",
                lib.tree_rows.len(),
                lib.shown.len()
            );
        }
        // Report median paint so a cold first frame does not hide the steady cost.
        let mut paints: Vec<f32> = samples.iter().map(|s| s.1).collect();
        paints.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med = paints[paints.len() / 2];
        eprintln!(
            "[LIB-BENCH] median_paint={med:.2}ms  (Gate 2 acceptance: <1.0 ms steady with 2278 assets)"
        );
        // Soft signal only: do not fail the suite on budget — owner measures the live app.
        // But a multi-second paint here would mean the microbench itself is broken.
        assert!(med < 5000.0, "headless paint absurdly slow: {med:.1} ms");
    }
}
