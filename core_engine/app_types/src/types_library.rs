// Content Library + Material Library types — extracted from types.rs
use eframe::egui;
use serde::{Serialize, Deserialize};
use core_math::primitives::PbrMaterial;

// ==================================================================
// Content Library
// ==================================================================
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum ContentCategory { Materials, HDRIs, Textures, MatCaps, Bodies }
impl ContentCategory {
    pub fn label(&self) -> &str {
        match self {
            Self::Materials => "Materials",
            Self::HDRIs => "HDRIs",
            Self::Textures => "Textures",
            Self::MatCaps => "MatCaps",
            Self::Bodies => "Bodies",
        }
    }
    pub fn icon(&self) -> &str {
        match self {
            Self::Materials => "\u{1F3A8}",
            Self::HDRIs => "\u{1F305}",
            Self::Textures => "\u{1F5BC}",
            Self::MatCaps => "\u{25C9}",
            Self::Bodies => "\u{2B21}",
        }
    }
    pub fn all() -> &'static [Self] {
        &[
            Self::Materials,
            Self::HDRIs,
            Self::Textures,
            Self::MatCaps,
            Self::Bodies,
        ]
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LibraryViewMode { DiskHierarchy, VirtualCategory }
#[derive(PartialEq,Clone,Copy,Debug)] pub enum ContentViewMode { Grid, List }

#[derive(Clone)]
pub struct ContentItem {
    pub id: u32,
    pub name: String,
    pub category: ContentCategory,
    pub folder_path: std::path::PathBuf,
    pub thumb_path: Option<std::path::PathBuf>,
    pub files: Vec<(String, std::path::PathBuf)>,
    pub thumb_handle: Option<egui::TextureHandle>,
    pub thumb_attempted: bool,
    pub tags: Vec<String>,
    pub favorite: bool,
    pub is_individual_file: bool,
    pub parent_folder: String,
    pub texture_role: String,
}

#[derive(Clone)]
pub struct LibraryDragPayload {
    pub item_idx: usize,
    pub category: ContentCategory,
}

#[derive(Clone)]
pub struct LibraryState {
    pub open: bool,
    pub height: f32,
    pub active_category: ContentCategory,
    pub search_query: String,
    pub items: Vec<ContentItem>,
    pub next_item_id: u32,
    pub scanned: bool,
    /// Cached `assets/` root display path (forever index cache / first scan).
    /// Avoids `find_asset_root()` walking the FS on every library paint frame.
    pub asset_root_display: String,
    pub resizing: bool,
    pub resize_start_y: f32,
    pub resize_start_height: f32,
    pub view_mode: LibraryViewMode,
    pub expanded_folders: std::collections::HashSet<String>,
    pub selected_folder: Option<String>,
    pub content_view: ContentViewMode,
    pub thumb_scale: f32,
    pub filter_role: Option<String>,
    pub matcap_preloaded: bool,
}
impl Default for LibraryState {
    fn default() -> Self {
        Self { open: false, height: 180.0, active_category: ContentCategory::Materials,
               search_query: String::new(), items: Vec::new(), next_item_id: 1,
               scanned: false, asset_root_display: String::new(),
               resizing: false, resize_start_y: 0.0,
               resize_start_height: 0.0, view_mode: LibraryViewMode::DiskHierarchy,
               expanded_folders: std::collections::HashSet::new(),
               selected_folder: None, content_view: ContentViewMode::Grid,
               thumb_scale: 1.0, filter_role: None, matcap_preloaded: false }
    }
}

// ============================================================
// Scene-level material library (SERIALIZATION ONLY)
// ============================================================
// These types exist solely for v4/v5 project file deserialization and
// backward-compatible v6 saves. Runtime material access goes through
// the scene graph (NodePayload::Material).

/// A material in the legacy scene library. Objects reference materials by `id`.
/// `id = 0` is reserved as "no material" (use viewport default shading).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SceneMaterial {
    pub id: u32,
    pub mat: PbrMaterial,
}

/// Legacy material library — kept for save/load backward compatibility.
/// Runtime material management uses scene graph `NodePayload::Material` nodes.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MaterialLibrary {
    pub materials: Vec<SceneMaterial>,
    #[serde(default)]
    pub next_id: u32,
}
