// Forever on-disk library INDEX cache — `%LOCALAPPDATA%\Azra\library_index.json`
//
// Stores the scanned catalog (names, categories, file paths, folders) so the library
// panel does not re-walk assets/ on every launch. Thumbnails stay separate
// (`thumbcache/`); runtime TextureHandles are never serialized.
//
// Lifetime:
//   • First open with no cache → full disk scan → save forever
//   • Later launches → load cache instantly (scanned=true)
//   • Rescan only when code clears `library.scanned` (rename/delete/add/refresh)
//     or cache is missing/corrupt/wrong version

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use crate::{ContentCategory, ContentItem};

/// Bump when the on-disk schema changes (forces one rescan).
const CACHE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct CachedFile {
    role: String,
    path: String,
}

#[derive(Serialize, Deserialize)]
struct CachedItem {
    id: u32,
    name: String,
    category: ContentCategory,
    folder_path: String,
    thumb_path: Option<String>,
    files: Vec<CachedFile>,
    tags: Vec<String>,
    favorite: bool,
    is_individual_file: bool,
    parent_folder: String,
    texture_role: String,
}

#[derive(Serialize, Deserialize)]
struct LibraryIndexFile {
    version: u32,
    next_item_id: u32,
    found_root: bool,
    /// Display string for the status bar (absolute assets root if known).
    asset_root: Option<String>,
    items: Vec<CachedItem>,
}

/// Loaded catalog ready to install into `LibraryState` (no GPU handles).
pub(crate) struct LoadedLibraryIndex {
    pub items: Vec<ContentItem>,
    pub next_item_id: u32,
    pub found_root: bool,
    pub asset_root_display: String,
}

fn azra_dir() -> PathBuf {
    std::env::var("LOCALAPPDATA")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .unwrap_or_else(std::env::temp_dir)
        })
        .join("Azra")
}

/// Path of the forever library index file.
pub(crate) fn library_index_cache_path() -> PathBuf {
    let dir = azra_dir();
    let _ = std::fs::create_dir_all(&dir);
    dir.join("library_index.json")
}

fn item_to_cached(item: &ContentItem) -> CachedItem {
    CachedItem {
        id: item.id,
        name: item.name.clone(),
        category: item.category,
        folder_path: item.folder_path.to_string_lossy().into_owned(),
        thumb_path: item
            .thumb_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        files: item
            .files
            .iter()
            .map(|(role, path)| CachedFile {
                role: role.clone(),
                path: path.to_string_lossy().into_owned(),
            })
            .collect(),
        tags: item.tags.clone(),
        favorite: item.favorite,
        is_individual_file: item.is_individual_file,
        parent_folder: item.parent_folder.clone(),
        texture_role: item.texture_role.clone(),
    }
}

fn cached_to_item(c: CachedItem) -> ContentItem {
    ContentItem {
        id: c.id,
        name: c.name,
        category: c.category,
        folder_path: PathBuf::from(c.folder_path),
        thumb_path: c.thumb_path.map(PathBuf::from),
        files: c
            .files
            .into_iter()
            .map(|f| (f.role, PathBuf::from(f.path)))
            .collect(),
        // Runtime-only — thumbs rehydrate via thumbcache / loader.
        thumb_handle: None,
        thumb_attempted: false,
        tags: c.tags,
        favorite: c.favorite,
        is_individual_file: c.is_individual_file,
        parent_folder: c.parent_folder,
        texture_role: c.texture_role,
    }
}

/// Load forever index. `None` = missing, corrupt, or wrong version → full scan.
pub(crate) fn load_library_index() -> Option<LoadedLibraryIndex> {
    let path = library_index_cache_path();
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(_) => return None,
    };
    let file: LibraryIndexFile = match serde_json::from_slice(&data) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "[LIB-CACHE] corrupt {:?} ({e}) — will rescan",
                path
            );
            return None;
        }
    };
    if file.version != CACHE_VERSION {
        eprintln!(
            "[LIB-CACHE] version {} != {} — will rescan",
            file.version, CACHE_VERSION
        );
        return None;
    }
    let n = file.items.len();
    let asset_root_display = file
        .asset_root
        .clone()
        .unwrap_or_else(|| "(cached library)".into());
    let items: Vec<ContentItem> = file.items.into_iter().map(cached_to_item).collect();
    eprintln!(
        "[LIB-CACHE] loaded {} items from {:?} (forever index)",
        n, path
    );
    Some(LoadedLibraryIndex {
        items,
        next_item_id: file.next_item_id.max(1),
        found_root: file.found_root || n > 0,
        asset_root_display,
    })
}

/// Write forever index after a successful full disk scan (or forced rescan).
pub(crate) fn save_library_index(
    items: &[ContentItem],
    next_item_id: u32,
    found_root: bool,
    asset_root: Option<&Path>,
) {
    let file = LibraryIndexFile {
        version: CACHE_VERSION,
        next_item_id,
        found_root,
        asset_root: asset_root.map(|p| p.to_string_lossy().into_owned()),
        items: items.iter().map(item_to_cached).collect(),
    };
    let path = library_index_cache_path();
    let tmp = path.with_extension("json.tmp");
    match serde_json::to_vec(&file) {
        Ok(bytes) => {
            let result = std::fs::write(&tmp, &bytes).and_then(|_| std::fs::rename(&tmp, &path));
            match result {
                Ok(()) => eprintln!(
                    "[LIB-CACHE] saved {} items ({} KB) → {:?}",
                    file.items.len(),
                    bytes.len() / 1024,
                    path
                ),
                Err(e) => {
                    eprintln!("[LIB-CACHE] WRITE FAILED: {e} — {:?}", path);
                    let _ = std::fs::remove_file(&tmp);
                }
            }
        }
        Err(e) => eprintln!("[LIB-CACHE] serialize failed: {e}"),
    }
}

/// Drop the forever index so the next `ensure_library_scanned` forces a disk walk.
#[allow(dead_code)]
pub(crate) fn invalidate_library_index() {
    let path = library_index_cache_path();
    match std::fs::remove_file(&path) {
        Ok(()) => eprintln!("[LIB-CACHE] invalidated {:?}", path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("[LIB-CACHE] invalidate failed: {e}"),
    }
}
