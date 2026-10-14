// core_engine/app/src/library_folders.rs
//
// User-configurable EXTRA library root folders (owner req #2). The owner keeps PBR
// material sets in an existing folder on disk and does NOT want to copy them into
// `~/.azra/library/`. This module persists a small list of extra root folders that the
// library scan walks IN ADDITION to the built-in roots (bundled `assets/` + the
// `~/.azra/library/` drop-folder).
//
// Persistence: `<~/.azra>/library_folders.json`, the same fail-soft JSON pattern as
// `ui2_layout.json` — versioned, missing/unreadable/wrong-version → an empty list, never
// a crash. No file duplication ever happens: only the paths are stored; the images stay
// where they are on disk.
//
// SCAN SEMANTICS (documented decision): a picked folder is scanned the SAME way as a
// built-in root — it is walked BOTH as a root (so `<picked>/PBR/<Name>/` sets are found,
// matching the built-in layout) AND, additionally, its direct `<Name>/` subfolders are
// treated as PBR material folders. The latter is the friendly case the owner asked for:
// the user points AZRA at the folder that DIRECTLY contains their material folders
// (e.g. `MyTextures/Brick/`, `MyTextures/Concrete/`) and each becomes a library material
// with no required `PBR/` nesting. Loose textures directly under the picked folder are
// also picked up. Unreadable/missing folders are skipped (fail-soft), never fatal.

use std::path::PathBuf;

/// On-disk schema version for `library_folders.json`.
const SCHEMA_VERSION: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
struct LibraryFoldersFile {
    v: u32,
    folders: Vec<String>,
}

/// Path to the persisted config (`~/.azra/library_folders.json`). Shares the AZRA config
/// dir with `keybinds.json` / `thumbcache/` (`KeyBindings::config_dir`).
fn config_path() -> Option<PathBuf> {
    Some(crate::KeyBindings::config_dir()?.join("library_folders.json"))
}

/// Load the persisted extra-folder list. Fail-soft: any error → empty list.
pub(crate) fn load() -> Vec<PathBuf> {
    let Some(path) = config_path() else { return Vec::new() };
    let Ok(json) = std::fs::read_to_string(&path) else { return Vec::new() };
    let Ok(file) = serde_json::from_str::<LibraryFoldersFile>(&json) else { return Vec::new() };
    if file.v != SCHEMA_VERSION { return Vec::new() }
    file.folders.into_iter().map(PathBuf::from).collect()
}

/// Persist the extra-folder list (best-effort; a write failure is non-fatal).
pub(crate) fn save(folders: &[PathBuf]) {
    let Some(path) = config_path() else { return };
    let file = LibraryFoldersFile {
        v: SCHEMA_VERSION,
        folders: folders.iter().map(|p| p.to_string_lossy().to_string()).collect(),
    };
    if let Ok(json) = serde_json::to_string_pretty(&file) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, json);
    }
}
