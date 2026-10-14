// core_engine/app/src/library_scan.rs
// Content-library open-scan WORKER — fixes the library-open stall (E3 investigation).
//
// The scan used to run synchronously inside `draw_content_library` on every library open:
// a full assets/ filesystem walk PLUS a full-res `image::open` of the first matcap PLUS
// `upload_matcap_to_gpu()` — a GPU buffer create FROM THE DRAW PATH (Frame Ordering violation).
// Now the walk + matcap decode run on a one-shot worker thread (the `request_env_load` /
// FileDialogService precedent: spawn + mpsc + drain in poll), and the ONLY GPU work — the matcap
// upload — happens in `poll_library_scan`, called from `poll_background_tasks` (Frame Ordering
// Contract). The library panel shows its previous items (or a "Scanning library…" placeholder on
// first open) until the result installs.
//
// The scan logic itself is UNCHANGED — it moved here verbatim from
// `content_library.rs::scan_content_library` (extracted to its own module to keep the
// content_library.rs diff surface minimal); the per-folder helpers
// (`scan_pbr_folder`/`scan_hdri_folder`/`scan_individual_images`/`find_asset_root`) still live
// there and are reused, not duplicated.

use std::sync::mpsc;
use crate::{AzraApp, ContentCategory, ContentItem};
use crate::content_library::{find_asset_root, scan_hdri_folder, scan_individual_images, scan_pbr_folder};

/// Result of one library scan worker run, installed by `poll_library_scan`.
pub(crate) struct LibraryScanResult {
    pub(crate) items: Vec<ContentItem>,
    pub(crate) next_item_id: u32,
    /// False when assets/ was not found (→ the old "Library: assets/ folder not found" toast).
    pub(crate) found_root: bool,
    /// First matcap, decoded off-thread (RGBA8, w, h) — only when the request said the app had
    /// none (`matcap_data` empty). GPU upload happens in poll, never here.
    pub(crate) matcap: Option<(Vec<u8>, u32, u32)>,
}

/// The user's writable library drop-folder — `~/.azra/library/` with the SAME
/// `PBR/ HDR_Labs/ Textures/ Matcaps/` structure as the bundled `assets/` root. This is where
/// downloaded PBR sets should go (owner #6): the bundled `assets/` root is exe-relative and not
/// present in an installed build, so it is not a place users can reliably drop content. Created on
/// demand (with a README) so it is discoverable the first time the library is opened.
fn user_library_root() -> Option<std::path::PathBuf> {
    let dir = crate::KeyBindings::config_dir()?.join("library");
    if !dir.is_dir() {
        let _ = std::fs::create_dir_all(dir.join("PBR"));
        let _ = std::fs::create_dir_all(dir.join("Textures"));
        let _ = std::fs::create_dir_all(dir.join("HDR_Labs"));
        let _ = std::fs::create_dir_all(dir.join("Matcaps"));
        let _ = std::fs::write(
            dir.join("README.txt"),
            "AZRA user library.\n\nDrop content here to have it appear in the in-app Library panel:\n  PBR/<MaterialName>/   one folder per material, holding its map images\n                        (basecolor / roughness / metallic / normal / ao / height),\n                        e.g. Brick_basecolor.jpg, Brick_normal.jpg, ...\n  Textures/             loose texture images (png / jpg / tga / bmp)\n  HDR_Labs/             .hdr / .exr environment maps\n  Matcaps/              matcap sphere images\n\nCatalog is cached forever in %LOCALAPPDATA%\\\\Azra\\\\library_index.json.\nAfter adding files here, use Refresh library (or rename/delete in-app) to rescan.\n",
        );
    }
    Some(dir)
}

/// The filesystem walk — worker-thread only. Scans BOTH the bundled exe-relative `assets/` root
/// and the user drop-folder (`~/.azra/library/`), merging their items (folder set per root:
/// PBR, HDR_Labs, Textures, Matcaps; same sort).
fn scan_library_items() -> LibraryScanResult {
    let mut items: Vec<ContentItem> = Vec::new();
    let mut next_item_id: u32 = 1;
    let mut found_root = false;

    if let Some(root) = find_asset_root() {
        found_root = true;
        scan_one_root(&root, &mut items, &mut next_item_id);
    }
    if let Some(uroot) = user_library_root() {
        // A present user library counts as "found" even if the bundled assets/ is missing
        // (installed builds), so the panel does not show the "assets/ not found" toast.
        found_root = true;
        scan_one_root(&uroot, &mut items, &mut next_item_id);
    }

    // User-picked extra library folders (owner req #2). Each is scanned BOTH as a root
    // (so `<picked>/PBR/<Name>/` sets work like the built-in layout) AND directly for
    // `<Name>/` material subfolders (the friendly case — user points at the folder that
    // directly holds their material folders). Missing/unreadable folders are skipped.
    for folder in crate::library_folders::load() {
        if !folder.is_dir() { continue; }
        found_root = true;
        scan_one_root(&folder, &mut items, &mut next_item_id);
        scan_folder_as_pbr_container(&folder, &mut items, &mut next_item_id);
    }

    // Kitbash body library (.flb) — same disk root as Save Body to Library.
    scan_kitbash_bodies(&mut items, &mut next_item_id);
    if !items.is_empty() {
        found_root = true;
    }

    items.sort_by(|a, b| a.category.label().cmp(b.category.label()).then(a.name.cmp(&b.name)));
    LibraryScanResult { items, next_item_id, found_root, matcap: None }
}

/// Scan `%LOCALAPPDATA%/Azra/library/**/*.flb` into ContentCategory::Bodies.
fn scan_kitbash_bodies(items: &mut Vec<ContentItem>, next_item_id: &mut u32) {
    let root = crate::project::library_path();
    if !root.is_dir() {
        return;
    }
    fn walk(
        dir: &std::path::Path,
        rel: &str,
        depth: usize,
        items: &mut Vec<ContentItem>,
        next_item_id: &mut u32,
    ) {
        if depth > 3 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = p
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                if name.starts_with('.') {
                    continue;
                }
                let next_rel = if rel.is_empty() {
                    name
                } else {
                    format!("{rel}/{name}")
                };
                walk(&p, &next_rel, depth + 1, items, next_item_id);
            } else if p.extension().map(|x| x == "flb").unwrap_or(false) {
                let stem = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let thumb = {
                    let t = dir.join(format!("{stem}.thumb.png"));
                    if t.is_file() {
                        Some(t)
                    } else {
                        let t2 = p.with_extension("thumb.png");
                        if t2.is_file() {
                            Some(t2)
                        } else {
                            None
                        }
                    }
                };
                let id = *next_item_id;
                *next_item_id += 1;
                let parent = if rel.is_empty() {
                    "Bodies".into()
                } else {
                    format!("Bodies/{rel}")
                };
                items.push(ContentItem {
                    id,
                    name: stem.replace('_', " "),
                    category: ContentCategory::Bodies,
                    folder_path: p.parent().unwrap_or(dir).to_path_buf(),
                    thumb_path: thumb,
                    files: vec![("body".into(), p)],
                    thumb_handle: None,
                    thumb_attempted: false,
                    tags: Vec::new(),
                    favorite: false,
                    is_individual_file: true,
                    parent_folder: parent,
                    texture_role: String::new(),
                });
            }
        }
    }
    walk(&root, "", 0, items, next_item_id);
}

/// Walk one library root (bundled `assets/` OR the user drop-folder). Appends to `items`.
fn scan_one_root(root: &std::path::Path, items: &mut Vec<ContentItem>, next_item_id: &mut u32) {
    // Scan PBR material folders
    let pbr_dir = root.join("PBR");
    if pbr_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&pbr_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() { continue; }
                if path.file_name().map_or(false, |n| n == "images") { continue; }
                let parent = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                // Material folder item
                if let Some(mut item) = scan_pbr_folder(&path, next_item_id) {
                    item.parent_folder = "PBR".into();
                    items.push(item);
                } else {
                    // Category folder (e.g. PBR/Metal/): one level of material subfolders
                    if let Ok(subs) = std::fs::read_dir(&path) {
                        for sub in subs.flatten() {
                            let sp = sub.path();
                            if !sp.is_dir() { continue; }
                            if let Some(mut item) = scan_pbr_folder(&sp, next_item_id) {
                                item.parent_folder = parent.clone();
                                items.push(item);
                            }
                        }
                    }
                }
                // Individual texture files
                scan_individual_images(&path, &parent, next_item_id, items);
            }
        }
    }

    // Scan HDRIs
    let hdr_dir = root.join("HDR_Labs");
    if hdr_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&hdr_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    for mut item in scan_hdri_folder(&path, next_item_id) {
                        item.parent_folder = "HDR_Labs".into();
                        items.push(item);
                    }
                } else if path.is_file() {
                    // Loose .hdr files directly in HDR_Labs/
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                    if ext == "hdr" {
                        let fname = path.file_name().unwrap_or_default().to_string_lossy();
                        let display = fname.trim_end_matches(".hdr").replace('_', " ");
                        let id = *next_item_id;
                        *next_item_id += 1;
                        items.push(ContentItem {
                            id, name: display.to_string(),
                            category: ContentCategory::HDRIs,
                            folder_path: hdr_dir.clone(),
                            thumb_path: None,
                            files: vec![("env".into(), path)],
                            thumb_handle: None, thumb_attempted: false,
                            tags: Vec::new(), favorite: false,
                            is_individual_file: false,
                            parent_folder: "HDR_Labs".into(),
                            texture_role: String::new(),
                        });
                    }
                }
            }
        }
    }

    // Scan loose Textures folder
    let tex_dir = root.join("Textures");
    if tex_dir.is_dir() {
        scan_individual_images(&tex_dir, "Textures", next_item_id, items);
    }

    // Scan MatCaps folder
    let matcap_dir = root.join("Matcaps");
    if matcap_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&matcap_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() { continue; }
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                // One source of truth for accepted image exts (matcaps included) — shared with the
                // PBR-set scanner so the accepted format list never drifts (README ↔ scan parity).
                if !crate::content_library::is_texture_map_ext(&ext) { continue; }
                let fname = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                let display = fname.rsplit_once('.').map(|(n, _)| n).unwrap_or(&fname).replace('_', " ");
                let id = *next_item_id;
                *next_item_id += 1;
                items.push(ContentItem {
                    id, name: display,
                    category: ContentCategory::MatCaps,
                    folder_path: matcap_dir.clone(),
                    thumb_path: Some(path.clone()),
                    files: vec![("matcap".into(), path)],
                    thumb_handle: None, thumb_attempted: false,
                    tags: Vec::new(), favorite: false,
                    is_individual_file: true,
                    parent_folder: "Matcaps".into(),
                    texture_role: String::new(),
                });
            }
        }
    }
}

/// Scan a user-picked folder whose direct subfolders ARE material folders (the friendly
/// case: the user points at `MyTextures/` that directly contains `Brick/`, `Concrete/`, …).
/// Reuses the SAME per-material logic as the `PBR/` root branch (`scan_pbr_folder` +
/// `scan_individual_images`) — no duplicated map-detection. The picked folder's own name is
/// the tree parent so its materials group under it. Complements `scan_one_root`, which
/// already covers a `<picked>/PBR/` layout; a material found by both is deduped by nothing,
/// so `scan_one_root` handles the `PBR/` case and this handles the direct-subfolder case
/// (a folder is not normally both).
fn scan_folder_as_pbr_container(
    root: &std::path::Path,
    items: &mut Vec<ContentItem>,
    next_item_id: &mut u32,
) {
    let parent = root.file_name().unwrap_or_default().to_string_lossy().to_string();
    let Ok(entries) = std::fs::read_dir(root) else { return; };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() { continue; }
        // Skip the reserved per-root layout dirs — those are handled by scan_one_root.
        if matches!(
            path.file_name().and_then(|n| n.to_str()),
            Some("PBR") | Some("HDR_Labs") | Some("Textures") | Some("Matcaps") | Some("images")
        ) { continue; }
        if let Some(mut item) = scan_pbr_folder(&path, next_item_id) {
            item.parent_folder = parent.clone();
            items.push(item);
        }
    }
    // Loose textures sitting directly in the picked folder.
    scan_individual_images(root, &parent, next_item_id, items);
}

/// Decode the first matcap of a scan result (worker thread — the full-res `image::open`
/// that used to stall the open). Mirrors the old auto-preload's "first MatCaps item" pick.
fn decode_first_matcap(items: &[ContentItem]) -> Option<(Vec<u8>, u32, u32)> {
    let idx = items.iter().position(|i| i.category == ContentCategory::MatCaps)?;
    let path = items[idx].files.iter()
        .find(|(role, _)| role == "matcap")
        .map(|(_, p)| p.clone())?;
    let img = image::open(&path).ok()?.to_rgba8();
    let (w, h) = (img.width(), img.height());
    Some((img.into_raw(), w, h))
}

impl AzraApp {
    /// Kick the library scan worker (idempotent while one is in flight). Called from the
    /// library panel draw — it only spawns a thread and stores the receiver; ALL state/GPU
    /// installation happens in `poll_library_scan`.
    pub(crate) fn request_library_scan(&mut self) {
        if self.library_scan_rx.is_some() { return; }
        let need_matcap = self.matcap_data.is_empty();
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("library-scan".into())
            .spawn(move || {
                let mut result = scan_library_items();
                if need_matcap {
                    result.matcap = decode_first_matcap(&result.items);
                }
                let _ = tx.send(result);
            })
            .is_ok();
        if spawned {
            self.library_scan_rx = Some(rx);
        }
        // spawn failure: rx dropped, scanned stays false → the next open retries.
    }

    /// Poll-phase install of a finished library scan: swap in the item list, mark `scanned`,
    /// save the forever disk index, and do the ONLY GPU work of the whole flow — the
    /// auto-preloaded matcap buffer upload (Frame Ordering Contract: poll is where GPU
    /// resources are created).
    pub(crate) fn poll_library_scan(&mut self, ctx: &eframe::egui::Context) {
        let Some(rx) = &self.library_scan_rx else { return };
        match rx.try_recv() {
            Ok(result) => {
                self.library_scan_rx = None;
                if !result.found_root {
                    self.toast("Library: assets/ folder not found");
                    self.library.items.clear();
                    self.library.next_item_id = 1;
                    self.library.scanned = true;
                    self.library.asset_root_display.clear();
                    crate::library_index_cache::save_library_index(
                        &[], 1, false, None,
                    );
                    return;
                }
                let asset_root = crate::content_library::find_asset_root();
                self.library.asset_root_display = asset_root
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(library)".into());
                self.library.items = result.items;
                self.library.next_item_id = result.next_item_id;
                self.library.scanned = true;
                // Forever catalog — next launches load without walking assets/.
                crate::library_index_cache::save_library_index(
                    &self.library.items,
                    self.library.next_item_id,
                    true,
                    asset_root.as_deref(),
                );
                // New items + favorites re-apply; folder/shown caches rebuild on next draw.
                self.ui2.library.invalidate_content_caches();
                // Refresh the grip-menu display copy of configured extra roots (owner #2)
                // off the just-completed scan — keeps the menu list in sync after add/remove
                // without any file I/O in the draw path.
                self.ui2.library.extra_folders = crate::library_folders::load();
                // Auto-preload first matcap so it's ready when the user switches to MatCap
                // mode — decoded on the worker; re-check emptiness (the user may have applied
                // one while the scan ran).
                if let Some((data, w, h)) = result.matcap {
                    if self.matcap_data.is_empty() {
                        self.matcap_data = data;
                        self.matcap_width = w;
                        self.matcap_height = h;
                        self.upload_matcap_to_gpu();
                    }
                }
                // DEFAULT PBR ENVIRONMENT (owner 2026-08-05: "can you make neutral dark 003
                // default pbr hdr"). A fresh session otherwise starts with NO environment, so
                // PBR shows no image-based lighting at all until the user hunts one down in
                // the library — and lookdev on a neutral grey gradient is the sane default for
                // hard-surface work.
                //
                // Only when nothing is loaded yet (`env_hdr_width == 0`): a project that
                // restored its own HDRI, or a user who already picked one while the scan ran,
                // must never be stomped. Goes through the same worker-decode + poll-install
                // path as a library click (Frame Ordering Contract — no GPU work here).
                if self.env_hdr_width == 0 {
                    if let Some(ref root) = asset_root {
                        let p = root.join("HDR_Labs/HDR_Gradient/neutral_dark_0.003.hdr");
                        if p.is_file() {
                            self.render.exposure = 1.0; // HDR input, same as apply_library_hdri
                            self.request_env_load(p);
                        }
                    }
                }
                let count = |c: ContentCategory| self.library.items.iter().filter(|i| i.category == c).count();
                self.toast(&format!(
                    "Library: {} materials, {} HDRIs, {} textures, {} matcaps, {} bodies",
                    count(ContentCategory::Materials), count(ContentCategory::HDRIs),
                    count(ContentCategory::Textures), count(ContentCategory::MatCaps),
                    count(ContentCategory::Bodies),
                ));
                ctx.request_repaint(); // show the freshly-installed rows this frame
            }
            Err(mpsc::TryRecvError::Empty) => {
                // Keep the placeholder animating / install promptly once the worker lands.
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // Worker died (panic) — never respawn-loop from draw: mark scanned with
                // whatever items we had and surface it.
                self.library_scan_rx = None;
                self.library.scanned = true;
                self.toast("Library scan failed — see log");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_library::{is_texture_map_ext, scan_pbr_folder};

    #[test]
    fn exr_and_tiff_are_accepted_map_exts() {
        for ext in ["jpg", "jpeg", "png", "exr", "tif", "tiff", "tga", "bmp"] {
            assert!(is_texture_map_ext(ext), "{ext} should be an accepted map ext");
        }
        assert!(!is_texture_map_ext("txt"));
        assert!(!is_texture_map_ext("hdr")); // env-only, not a PBR channel map
    }

    #[test]
    fn picked_folder_with_exr_material_subfolder_becomes_library_item() {
        // A user-picked folder that DIRECTLY contains a material folder holding .exr maps.
        let root = std::env::temp_dir().join(format!("azra_libtest_{}", std::process::id()));
        let mat = root.join("Brick");
        std::fs::create_dir_all(&mat).unwrap();
        // scan_pbr_folder inspects extension + filename only (no decode), so empty files suffice.
        std::fs::write(mat.join("Brick_basecolor.exr"), b"").unwrap();
        std::fs::write(mat.join("Brick_normal.exr"), b"").unwrap();

        let mut items = Vec::new();
        let mut next_id = 1u32;
        scan_folder_as_pbr_container(&root, &mut items, &mut next_id);

        let brick = items
            .iter()
            .find(|i| i.name == "Brick" && i.category == crate::ContentCategory::Materials)
            .expect("Brick material from the picked folder");
        assert!(brick.files.iter().any(|(role, _)| role == "basecolor"));
        assert_eq!(brick.parent_folder, root.file_name().unwrap().to_string_lossy());

        // Sanity: scan_pbr_folder alone also accepts the exr maps.
        let mut nid = 1u32;
        assert!(scan_pbr_folder(&mat, &mut nid).is_some());

        let _ = std::fs::remove_dir_all(&root);
    }
}
