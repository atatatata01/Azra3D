//! Deferred file dialogs — the rfd modal-in-frame crash-class fix (2026-07-18).
//!
//! `rfd::FileDialog::pick_file()` used to run MODALLY on the UI thread in the middle of an
//! egui frame at every map-load site. On Windows the modal dialog pumps messages re-entrantly
//! into the still-borrowed eframe state — a known BorrowMutError crash class (flagged in
//! MATERIALS_UX.md §E4.3 as the prime suspect for the owner's unexplained crash).
//!
//! Structural fix: ONE service all sites share. The dialog runs on its OWN thread (rfd
//! initializes COM per-call, so worker-thread dialogs are supported); the heavy follow-up
//! work (image decode) also happens on that thread; the result is delivered through a
//! channel and consumed in `poll_background_tasks` (Frame Ordering Contract — the GPU
//! uploads it feeds already happen in poll).
//!
//! One dialog at a time: `busy` latches while a picker is open; further requests toast.

use crate::*;
use crate::material_textures::{MapTarget, PendingMapLoad};
use core_math::pbr_material::MapKind;

/// What the user is picking a file FOR. Carried to the worker and back.
pub(crate) enum DialogPurpose {
    /// A material map channel: decode on the worker (`decode_file_to_map` /
    /// `decode_file_to_data_map` by `kind.is_data()`), then queue the standard
    /// `PendingMapLoad` for the poll-phase upload.
    MaterialMap { target: MapTarget, kind: MapKind },
    /// An HDRI environment: the path is handed to the env worker
    /// (`request_env_load` — decode + mips + SH9 off-thread).
    EnvHdri,
    /// AI material detection reference image: the worker reads the bytes; poll
    /// forwards them to the AI service (the 5th former in-frame rfd site).
    AnalyzeImage,
    /// Sculpt brush-tip alpha: multi-file pick (images / ABR / .blend).
    BrushTip,
    /// Import an entire folder of alphas (Blender / 3D-Coat / Substance packs).
    BrushTipFolder,
    /// Scan Blender Program Files essentials brush `.blend` packs (no picker).
    BrushTipBlenderEssentials,
}

/// Worker → poll delivery.
pub(crate) enum DialogOutcome {
    MaterialMap { load: PendingMapLoad },
    MaterialMapFailed { name: String },
    EnvPicked { path: std::path::PathBuf },
    AnalyzeImageBytes { bytes: Vec<u8> },
    /// One or more tips imported; first is installed as active when present.
    BrushTipsImported {
        /// Tips written this import (name, path, tip) — first becomes active.
        tips: Vec<(String, String, core_math::voxel_sculpt::BrushTipAlpha)>,
        ok: usize,
        err: usize,
        source: String,
    },
    BrushTipFailed { name: String },
    Cancelled,
}

pub(crate) struct FileDialogService {
    tx: std::sync::mpsc::Sender<DialogOutcome>,
    rx: std::sync::mpsc::Receiver<DialogOutcome>,
    /// True while a picker thread is open (cleared when its outcome is drained).
    busy: bool,
}

impl FileDialogService {
    pub(crate) fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self { tx, rx, busy: false }
    }

    /// Open a file picker on a worker thread. `filters` = (label, extensions).
    /// Returns false (and does nothing) if a picker is already open.
    pub(crate) fn request(
        &mut self,
        title: &str,
        filters: &[(&str, &[&str])],
        purpose: DialogPurpose,
    ) -> bool {
        if self.busy {
            return false;
        }
        self.busy = true;
        let tx = self.tx.clone();
        let title = title.to_string();
        let filters: Vec<(String, Vec<String>)> = filters
            .iter()
            .map(|(label, exts)| {
                (label.to_string(), exts.iter().map(|e| e.to_string()).collect())
            })
            .collect();
        std::thread::spawn(move || {
            let outcome = match purpose {
                DialogPurpose::BrushTipFolder => {
                    let mut dialog = rfd::FileDialog::new().set_title(&title);
                    match dialog.pick_folder() {
                        None => DialogOutcome::Cancelled,
                        Some(folder) => match crate::brush_tip_lib::import_folder_to_library(&folder) {
                            Ok((ok, err, first)) => {
                                let mut tips = Vec::new();
                                if let Some((n, p, t)) = first {
                                    tips.push((n, p.to_string_lossy().to_string(), t));
                                }
                                DialogOutcome::BrushTipsImported {
                                    tips,
                                    ok,
                                    err,
                                    source: folder
                                        .file_name()
                                        .and_then(|s| s.to_str())
                                        .unwrap_or("folder")
                                        .to_string(),
                                }
                            }
                            Err(e) => DialogOutcome::BrushTipFailed { name: e },
                        },
                    }
                }
                DialogPurpose::BrushTipBlenderEssentials => {
                    match crate::brush_tip_lib::import_blender_essentials_tips() {
                        Ok((ok, err, first)) => {
                            let mut tips = Vec::new();
                            if let Some((n, p, t)) = first {
                                tips.push((n, p.to_string_lossy().to_string(), t));
                            }
                            DialogOutcome::BrushTipsImported {
                                tips,
                                ok,
                                err,
                                source: "Blender essentials".into(),
                            }
                        }
                        Err(e) => DialogOutcome::BrushTipFailed { name: e },
                    }
                }
                DialogPurpose::BrushTip => {
                    let mut dialog = rfd::FileDialog::new().set_title(&title);
                    for (label, exts) in &filters {
                        let ext_refs: Vec<&str> = exts.iter().map(|s| s.as_str()).collect();
                        dialog = dialog.add_filter(label, &ext_refs);
                    }
                    match dialog.pick_files() {
                        None => DialogOutcome::Cancelled,
                        Some(paths) if paths.is_empty() => DialogOutcome::Cancelled,
                        Some(paths) => {
                            let mut tips = Vec::new();
                            let mut ok = 0usize;
                            let mut err = 0usize;
                            for path in &paths {
                                match crate::brush_tip_lib::import_tips_from_path(path) {
                                    Ok(list) => {
                                        ok += list.len();
                                        for (n, p, t) in list {
                                            tips.push((n, p.to_string_lossy().to_string(), t));
                                        }
                                    }
                                    Err(_) => err += 1,
                                }
                            }
                            if tips.is_empty() {
                                let name = paths
                                    .first()
                                    .and_then(|p| p.file_name())
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("tip")
                                    .to_string();
                                DialogOutcome::BrushTipFailed {
                                    name: format!("{name} (decoded 0 tips)"),
                                }
                            } else {
                                DialogOutcome::BrushTipsImported {
                                    tips,
                                    ok,
                                    err,
                                    source: format!("{} file(s)", paths.len()),
                                }
                            }
                        }
                    }
                }
                other => {
                    let mut dialog = rfd::FileDialog::new().set_title(&title);
                    for (label, exts) in &filters {
                        let ext_refs: Vec<&str> = exts.iter().map(|s| s.as_str()).collect();
                        dialog = dialog.add_filter(label, &ext_refs);
                    }
                    match dialog.pick_file() {
                        None => DialogOutcome::Cancelled,
                        Some(path) => match other {
                            DialogPurpose::EnvHdri => DialogOutcome::EnvPicked { path },
                            DialogPurpose::AnalyzeImage => match std::fs::read(&path) {
                                Ok(bytes) => DialogOutcome::AnalyzeImageBytes { bytes },
                                Err(_) => DialogOutcome::Cancelled,
                            },
                            DialogPurpose::MaterialMap { target, kind } => {
                                // Decode HERE (worker). Displacement / Bump: EXR/float keep full
                                // precision → dual-plane 16-bit height. Other channels: 8-bit plane.
                                let name = crate::material_textures::map_display_name(&path);
                                let height_kind =
                                    matches!(kind, MapKind::Displacement | MapKind::Bump);
                                if height_kind {
                                    match crate::material_textures::decode_file_to_height16_planes(&path)
                                    {
                                        Some((hi, lo)) => DialogOutcome::MaterialMap {
                                            load: PendingMapLoad {
                                                target,
                                                kind,
                                                pixels: hi,
                                                strength: kind.default_strength(),
                                                name,
                                                high_precision: true,
                                                pixels_lo: lo,
                                                green_dx: false,
                                            },
                                        },
                                        None => DialogOutcome::MaterialMapFailed { name },
                                    }
                                } else {
                                    let pixels = if kind.is_data() {
                                        crate::material_textures::decode_file_to_data_map(&path)
                                    } else {
                                        crate::material_textures::decode_file_to_map(&path)
                                    };
                                    // Hand-loaded normal maps get the same GL/DX auto-detect
                                    // as the library import (worker thread — probes the folder).
                                    let green_dx = kind == MapKind::Normal
                                        && crate::material_textures::normal_green_is_dx(&path);
                                    match pixels {
                                        Some(pixels) => DialogOutcome::MaterialMap {
                                            load: PendingMapLoad {
                                                target,
                                                kind,
                                                pixels,
                                                strength: kind.default_strength(),
                                                name,
                                                high_precision: false,
                                                pixels_lo: Vec::new(),
                                                green_dx,
                                            },
                                        },
                                        None => DialogOutcome::MaterialMapFailed { name },
                                    }
                                }
                            }
                            // Handled above
                            DialogPurpose::BrushTip
                            | DialogPurpose::BrushTipFolder
                            | DialogPurpose::BrushTipBlenderEssentials => {
                                DialogOutcome::Cancelled
                            }
                        },
                    }
                }
            };
            let _ = tx.send(outcome);
        });
        true
    }


    /// Decode a KNOWN image path (no picker) on a worker thread and deliver it as a
    /// `DialogOutcome::MaterialMap` through the same channel the file picker uses. Backs the
    /// library-drag → texture-slot drop (owner #7): the drop already knows the file path, so it
    /// skips the picker but reuses the identical decode + poll-drain + hand-loaded-ownership path.
    /// Always spawns (a drop is user-initiated and one-shot); does not consult `busy`.
    pub(crate) fn load_map_from_path(
        &self,
        path: std::path::PathBuf,
        target: MapTarget,
        kind: MapKind,
    ) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let name = crate::material_textures::map_display_name(&path);
            let height_kind = matches!(kind, MapKind::Displacement | MapKind::Bump);
            let outcome = if height_kind {
                match crate::material_textures::decode_file_to_height16_planes(&path) {
                    Some((hi, lo)) => DialogOutcome::MaterialMap {
                        load: PendingMapLoad {
                            target,
                            kind,
                            pixels: hi,
                            strength: kind.default_strength(),
                            name,
                            high_precision: true,
                            pixels_lo: lo,
                            green_dx: false,
                        },
                    },
                    None => DialogOutcome::MaterialMapFailed { name },
                }
            } else {
                let pixels = if kind.is_data() {
                    crate::material_textures::decode_file_to_data_map(&path)
                } else {
                    crate::material_textures::decode_file_to_map(&path)
                };
                // Same GL/DX auto-detect as the picker path above.
                let green_dx = kind == MapKind::Normal
                    && crate::material_textures::normal_green_is_dx(&path);
                match pixels {
                    Some(pixels) => DialogOutcome::MaterialMap {
                        load: PendingMapLoad {
                            target,
                            kind,
                            pixels,
                            strength: kind.default_strength(),
                            name,
                            high_precision: false,
                            pixels_lo: Vec::new(),
                            green_dx,
                        },
                    },
                    None => DialogOutcome::MaterialMapFailed { name },
                }
            };
            let _ = tx.send(outcome);
        });
    }
}

/// Extensions the material-map picker accepts. ONE list, so the "Load…" dialog and the
/// Explorer drag-drop target can never disagree about what a map image is.
pub(crate) const MAP_IMAGE_EXTS: [&str; 6] = ["png", "jpg", "jpeg", "tga", "bmp", "webp"];

/// True when `p` looks like a material map image.
///
/// NOTE (flagged, not refactored — CLAUDE.md refactor budget): this extension list is
/// duplicated at brush_tip_lib.rs:113/317/890, content_library.rs:970 and
/// win_texture.rs:501 with three DIFFERENT sets. They should all come through here.
pub(crate) fn is_map_image_path(p: &std::path::Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| MAP_IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

impl AzraApp {
    /// Shared entry for every material-map "Load…" button (the 5 former in-frame rfd
    /// sites). Toasts if another picker is already open.
    pub(crate) fn request_map_file_dialog(&mut self, kind: MapKind, target: MapTarget) {
        let title = format!("Load {} Map", kind.label());
        let opened = self.file_dialogs.request(
            &title,
            &[("Image", &MAP_IMAGE_EXTS)],
            DialogPurpose::MaterialMap { target, kind },
        );
        if !opened {
            self.toast("A file dialog is already open");
        }
    }

    /// The HDRI environment "Load…" button (render panel).
    pub(crate) fn request_env_file_dialog(&mut self) {
        let opened = self.file_dialogs.request(
            "Load HDR Environment Map",
            &[("HDRI", &["hdr", "exr"]), ("Image", &["png", "jpg", "jpeg"])],
            DialogPurpose::EnvHdri,
        );
        if !opened {
            self.toast("A file dialog is already open");
        }
    }

    /// Sculpt panel "Import Tip…" — multi-select PNG/JPG/TGA + ABR + Blender .blend.
    pub(crate) fn request_brush_tip_file_dialog(&mut self) {
        let opened = self.file_dialogs.request(
            "Import Brush Tips (images / ABR / Blender .blend)",
            &[
                (
                    "Images + Blender + ABR",
                    &[
                        "png", "jpg", "jpeg", "tga", "bmp", "webp", "exr", "abr", "blend",
                    ],
                ),
                ("Images", &["png", "jpg", "jpeg", "tga", "bmp", "webp", "exr"]),
                ("Blender brush packs", &["blend"]),
                ("Photoshop Brushes", &["abr"]),
            ],
            DialogPurpose::BrushTip,
        );
        if !opened {
            self.toast("A file dialog is already open");
        }
    }

    /// Import a whole folder of alpha images (Blender / 3D-Coat / Substance style).
    pub(crate) fn request_brush_tip_folder_dialog(&mut self) {
        let opened = self.file_dialogs.request(
            "Import Tip Folder (PNG alphas…)",
            &[],
            DialogPurpose::BrushTipFolder,
        );
        if !opened {
            self.toast("A file dialog is already open");
        }
    }

    /// Scan installed Blender for essentials sculpt/texture brush `.blend` packs.
    pub(crate) fn request_brush_tip_blender_essentials(&mut self) {
        let opened = self.file_dialogs.request(
            "Import Blender Essentials",
            &[],
            DialogPurpose::BrushTipBlenderEssentials,
        );
        if !opened {
            self.toast("A file dialog is already open");
        }
    }

    /// Poll-phase drain: route dialog outcomes into the standard queues.
    pub(crate) fn poll_file_dialogs(&mut self) {
        loop {
            let outcome = match self.file_dialogs.rx.try_recv() {
                Ok(o) => o,
                Err(_) => break,
            };
            self.file_dialogs.busy = false;
            match outcome {
                DialogOutcome::Cancelled => {}
                DialogOutcome::MaterialMap { load } => {
                    self.cache.material_map_loads.push(load);
                }
                DialogOutcome::MaterialMapFailed { name } => {
                    self.toast(&format!("Could not decode image: {name}"));
                }
                DialogOutcome::EnvPicked { path } => {
                    self.request_env_load(path);
                }
                DialogOutcome::AnalyzeImageBytes { bytes } => {
                    self.ai
                        .ai_service
                        .send(crate::ai_client::AiRequest::AnalyzeImage { image_bytes: bytes });
                }
                DialogOutcome::BrushTipsImported {
                    tips,
                    ok,
                    err,
                    source,
                } => {
                    self.tip_preview_cache.invalidate();
                    if let Some((name, path, tip)) = tips.into_iter().next() {
                        let dims = format!("{}×{}", tip.width, tip.height);
                        let extra = if err > 0 {
                            format!(" · {err} failed")
                        } else {
                            String::new()
                        };
                        // Install onto whichever sculpt target is live: voxel first, else the
                        // poly tip (PolySculpt samples tips too since 2026-08-19).
                        if let Some(v) = self.sculpt.voxel.as_mut() {
                            v.brush_tip = Some(tip);
                            v.brush_tip_name = name.clone();
                            v.brush_tip_path = Some(path);
                            self.toast(&format!(
                                "Imported {ok} tip(s) from {source} · active: {name} ({dims}){extra}"
                            ));
                        } else if self.sculpt.poly_edit.is_some()
                            || self.poly_sculpt_target().is_some()
                        {
                            self.sculpt.poly_tip = Some(tip);
                            self.sculpt.poly_tip_name = name.clone();
                            self.sculpt.poly_tip_path = Some(path);
                            self.toast(&format!(
                                "Imported {ok} tip(s) from {source} · active: {name} ({dims}){extra}"
                            ));
                        } else {
                            self.toast(&format!(
                                "Imported {ok} tip(s) from {source} (no active sculpt — pick from library)"
                            ));
                        }
                    } else {
                        self.toast(&format!("Imported {ok} tip(s) from {source}"));
                    }
                }
                DialogOutcome::BrushTipFailed { name } => {
                    self.toast(&format!("Could not import brush tips: {name}"));
                }
            }
        }
    }
}
