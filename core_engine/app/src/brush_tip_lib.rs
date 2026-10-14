//! Persistent brush-tip library under `%LOCALAPPDATA%/Azra/brush_tips/`.
//!
//! Import sources:
//! - Greyscale / RGB images: PNG / JPG / TGA / BMP / WebP / EXR (luma)
//! - Photoshop ABR (sampled 8-bit brushes)
//! - Blender `.blend` assets (zstd/gzip/raw) — extracts packed PNG/JPEG streams
//! - Whole folders of alpha images (Blender / 3D-Coat / Substance style packs)
//! - **Procedural**: SDF shapes (`SdfShape::distance`) + shader-style FBM noise → greyscale tips
//!
//! Tips are stored as greyscale PNG so they survive restarts and Save As paths stay valid.
//! UI previews use [`tip_preview_rgba`] / [`BrushTipPreviewCache`].

use core_math::primitives::SdfShape;
use core_math::sdf_shapes::BoxBevel;
use core_math::voxel_sculpt::BrushTipAlpha;
use glam::Vec3;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

const TIP_RES: u32 = 128;
/// UI thumbnail resolution (square).
pub(crate) const TIP_THUMB_PX: u32 = 48;

/// One library entry (display name + on-disk path).
#[derive(Clone, Debug)]
pub(crate) struct TipLibraryEntry {
    pub name: String,
    pub path: PathBuf,
}

/// `%LOCALAPPDATA%/Azra/brush_tips`
pub(crate) fn tip_library_dir() -> Option<PathBuf> {
    let base = dirs::data_local_dir()?;
    let dir = base.join("Azra").join("brush_tips");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

/// List imported tip basenames (without extension), sorted.
pub(crate) fn list_library_tips() -> Vec<String> {
    list_library_entries()
        .into_iter()
        .map(|e| e.name)
        .collect()
}

/// One-shot flag: procedural essentials install only runs once per process (not every UI frame).
static PROC_TIPS_ENSURED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Invalidation generation for tip library list cache (bump after import/save/delete).
static TIP_LIST_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Call after any tip library write so the sculpt panel list refreshes without per-frame readdir.
pub(crate) fn invalidate_tip_library_list() {
    TIP_LIST_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// List library entries with full paths (sorted by name).
/// Ensures procedural essentials **once** (not every frame — was a sculpt-panel hitch).
/// Results are cached until [`invalidate_tip_library_list`].
pub(crate) fn list_library_entries() -> Vec<TipLibraryEntry> {
    use std::sync::{Mutex, OnceLock};
    struct Cache {
        gen: u64,
        entries: Vec<TipLibraryEntry>,
    }
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        Mutex::new(Cache {
            gen: 0,
            entries: Vec::new(),
        })
    });

    if !PROC_TIPS_ENSURED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        let _ = ensure_procedural_essentials();
        invalidate_tip_library_list();
    }

    let gen = TIP_LIST_GEN.load(std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut g) = cache.lock() {
        if g.gen == gen {
            return g.entries.clone();
        }
        let entries = scan_tip_library_dir();
        g.gen = gen;
        g.entries = entries.clone();
        return entries;
    }
    scan_tip_library_dir()
}

fn scan_tip_library_dir() -> Vec<TipLibraryEntry> {
    let Some(dir) = tip_library_dir() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let ext = p
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "tga" | "bmp" | "webp"
        ) {
            continue;
        }
        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
            if !stem.is_empty() {
                out.push(TipLibraryEntry {
                    name: stem.to_string(),
                    path: p,
                });
            }
        }
    }
    out.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));
    out.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    out
}

fn sanitize_stem(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim()
        .chars()
        .take(80)
        .collect()
}

fn unique_stem(dir: &Path, base: &str) -> String {
    let base = sanitize_stem(base);
    let base = if base.is_empty() {
        "tip".to_string()
    } else {
        base
    };
    let candidate = dir.join(format!("{base}.png"));
    if !candidate.exists() {
        return base;
    }
    for i in 2..=9999 {
        let name = format!("{base}-{i:03}");
        if !dir.join(format!("{name}.png")).exists() {
            return name;
        }
    }
    format!("{base}-x")
}

/// Decode any supported tip source (image, ABR, or .blend) → BrushTipAlpha (≤128² greyscale).
pub(crate) fn decode_tip_file(path: &Path) -> Option<BrushTipAlpha> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "abr" => decode_abr_largest_sample(path),
        "blend" => extract_blend_images(path)
            .ok()
            .and_then(|v| v.into_iter().next())
            .map(|(_, tip)| tip),
        _ => decode_image_file(path),
    }
}

fn decode_image_file(path: &Path) -> Option<BrushTipAlpha> {
    let img = image::open(path).ok()?;
    let img = img.resize(TIP_RES, TIP_RES, image::imageops::FilterType::Lanczos3);
    let gray = img.to_luma8();
    let (w, h) = gray.dimensions();
    BrushTipAlpha::from_luma_u8(w, h, gray.as_raw())
}

fn decode_image_bytes(bytes: &[u8]) -> Option<BrushTipAlpha> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = img.resize(TIP_RES, TIP_RES, image::imageops::FilterType::Lanczos3);
    let gray = img.to_luma8();
    let (w, h) = gray.dimensions();
    BrushTipAlpha::from_luma_u8(w, h, gray.as_raw())
}

/// Import one file into the library. ABR / .blend may import multiple tips; returns the **first**.
pub(crate) fn import_tip_to_library(src: &Path) -> Result<(String, PathBuf, BrushTipAlpha), String> {
    let imported = import_tips_from_path(src)?;
    imported
        .into_iter()
        .next()
        .ok_or_else(|| "Import produced no tips".into())
}

/// Import one path (image / ABR / .blend). Returns all tips written to the library.
pub(crate) fn import_tips_from_path(src: &Path) -> Result<Vec<(String, PathBuf, BrushTipAlpha)>, String> {
    let dir = tip_library_dir().ok_or_else(|| "No app data directory".to_string())?;
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let base = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("tip");
    let base = sanitize_stem(base);
    if base.is_empty() {
        return Err("Empty tip name".into());
    }

    match ext.as_str() {
        "abr" => {
            let samples = decode_abr_all_samples(src).ok_or_else(|| {
                "Could not decode Photoshop ABR (need sampled 8-bit brushes)".to_string()
            })?;
            if samples.is_empty() {
                return Err("ABR contains no usable sampled brush tips".into());
            }
            let mut out = Vec::new();
            for (i, tip) in samples.into_iter().enumerate() {
                let stem_base = if i == 0 {
                    base.clone()
                } else {
                    format!("{base}-{:03}", i + 1)
                };
                let name = unique_stem(&dir, &stem_base);
                let path = write_tip_png(&dir, &name, &tip)?;
                out.push((name, path, tip));
            }
            Ok(out)
        }
        "blend" => {
            let images = extract_blend_images(src).map_err(|e| e)?;
            if images.is_empty() {
                return Err(
                    "No PNG/JPEG images found in .blend (file may be empty or not pack textures)"
                        .into(),
                );
            }
            let mut out = Vec::new();
            for (i, (label, tip)) in images.into_iter().enumerate() {
                let stem_base = if label.is_empty() {
                    if i == 0 {
                        base.clone()
                    } else {
                        format!("{base}-{:03}", i + 1)
                    }
                } else {
                    format!("{base}_{label}")
                };
                let name = unique_stem(&dir, &stem_base);
                let path = write_tip_png(&dir, &name, &tip)?;
                out.push((name, path, tip));
            }
            Ok(out)
        }
        _ => {
            let tip = decode_image_file(src).ok_or_else(|| {
                format!("Could not decode image (use PNG/JPG/TGA/BMP/WebP/EXR). Got .{ext}")
            })?;
            let name = unique_stem(&dir, &base);
            let path = write_tip_png(&dir, &name, &tip)?;
            Ok(vec![(name, path, tip)])
        }
    }
}

/// Recursively import every supported image / ABR / blend under `folder`.
/// Returns (ok_count, err_count, first_tip).
pub(crate) fn import_folder_to_library(
    folder: &Path,
) -> Result<(usize, usize, Option<(String, PathBuf, BrushTipAlpha)>), String> {
    if !folder.is_dir() {
        return Err("Not a folder".into());
    }
    let mut ok = 0usize;
    let mut err = 0usize;
    let mut first = None;
    let mut stack = vec![folder.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                // Skip huge / irrelevant trees
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if name.starts_with('.') || name.eq_ignore_ascii_case("__pycache__") {
                    continue;
                }
                stack.push(p);
                continue;
            }
            let ext = p
                .extension()
                .and_then(|x| x.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !matches!(
                ext.as_str(),
                "png" | "jpg" | "jpeg" | "tga" | "bmp" | "webp" | "exr" | "abr" | "blend"
            ) {
                continue;
            }
            match import_tips_from_path(&p) {
                Ok(list) => {
                    ok += list.len();
                    if first.is_none() {
                        first = list.into_iter().next();
                    }
                }
                Err(_) => err += 1,
            }
        }
    }
    if ok == 0 && err == 0 {
        return Err("No supported tip images found in folder".into());
    }
    Ok((ok, err, first))
}

/// Install built-in procedural essentials (SDF shapes + noise) into the tip library if missing.
/// Stock Blender brushes are procedural (no PNG in .blend) — this is our port of that idea.
/// Returns how many new tips were written.
pub(crate) fn ensure_procedural_essentials() -> Result<usize, String> {
    let dir = tip_library_dir().ok_or_else(|| "No app data directory".to_string())?;
    let mut wrote = 0usize;
    for (name, tip) in procedural_essentials_catalog() {
        let path = dir.join(format!("{name}.png"));
        if path.exists() {
            continue;
        }
        write_tip_png(&dir, &name, &tip)?;
        wrote += 1;
    }
    if wrote > 0 {
        invalidate_tip_library_list();
    }
    Ok(wrote)
}

/// Force-rewrite procedural essentials (overwrites existing same-named PNGs).
pub(crate) fn install_procedural_essentials_force() -> Result<usize, String> {
    let dir = tip_library_dir().ok_or_else(|| "No app data directory".to_string())?;
    let mut wrote = 0usize;
    for (name, tip) in procedural_essentials_catalog() {
        write_tip_png(&dir, &name, &tip)?;
        wrote += 1;
    }
    invalidate_tip_library_list();
    Ok(wrote)
}

/// Catalog of default procedural tip names + alphas (SDF + classic shapes).
pub(crate) fn procedural_essentials_catalog() -> Vec<(String, BrushTipAlpha)> {
    let res = TIP_RES;
    let mut v = Vec::new();
    // SDF-backed (uses production `SdfShape::distance`)
    for kind in ProcAlphaKind::SDF_ALL {
        let p = ProcAlphaParams {
            kind,
            ..ProcAlphaParams::default()
        };
        v.push((kind.file_stem().to_string(), generate_proc_alpha(&p)));
    }
    // Classic procedural (also in catalog for completeness)
    v.push(("Soft Round".into(), BrushTipAlpha::soft_round(res)));
    v.push(("Hard Round".into(), BrushTipAlpha::hard_round(res)));
    v.push(("Clay Strips".into(), BrushTipAlpha::clay_strips(res)));
    v.push(("Soft Ellipse".into(), BrushTipAlpha::soft_ellipse(res, 1.6)));
    v.push(("Thin Line".into(), BrushTipAlpha::thin_line_crease(res, 0.055)));
    v.push(("Square Soft".into(), BrushTipAlpha::square_clay(res, 0.18)));
    v.push(("Square Hard".into(), BrushTipAlpha::square_clay(res, 0.05)));
    v.push(("Soft Blob".into(), BrushTipAlpha::soft_blob(res)));
    v.push(("Ring".into(), BrushTipAlpha::ring(res)));
    v.push(("V Groove".into(), BrushTipAlpha::v_groove(res)));
    v.push(("Dots".into(), BrushTipAlpha::dots(res)));
    v.push(("Soft Noise".into(), BrushTipAlpha::soft_noise(res, 42)));
    v
}

/// Try known Blender install paths for essentials brush `.blend` packs and import images.
/// If packs have no PNG streams (stock procedural brushes), installs our procedural SDF essentials.
pub(crate) fn import_blender_essentials_tips(
) -> Result<(usize, usize, Option<(String, PathBuf, BrushTipAlpha)>), String> {
    let mut roots = Vec::new();
    // Program Files installs
    for base in [
        r"C:\Program Files\Blender Foundation",
        r"C:\Program Files (x86)\Blender Foundation",
    ] {
        let p = PathBuf::from(base);
        if p.is_dir() {
            roots.push(p);
        }
    }
    // Portable / user
    if let Ok(home) = std::env::var("LOCALAPPDATA") {
        roots.push(PathBuf::from(home).join("Programs"));
    }

    let mut blend_files = Vec::new();
    for root in roots {
        let Ok(rd) = std::fs::read_dir(&root) else {
            continue;
        };
        for e in rd.flatten() {
            let blender_dir = e.path();
            // …/Blender X.Y/X.Y/datafiles/assets/brushes/*.blend
            if let Ok(ver_rd) = std::fs::read_dir(&blender_dir) {
                for ve in ver_rd.flatten() {
                    let brushes = ve
                        .path()
                        .join("datafiles")
                        .join("assets")
                        .join("brushes");
                    if brushes.is_dir() {
                        if let Ok(br) = std::fs::read_dir(&brushes) {
                            for b in br.flatten() {
                                let p = b.path();
                                if p.extension().and_then(|x| x.to_str()) == Some("blend") {
                                    // Prefer mesh_sculpt / mesh_texture packs
                                    let name = p
                                        .file_name()
                                        .and_then(|s| s.to_str())
                                        .unwrap_or("")
                                        .to_ascii_lowercase();
                                    if name.contains("sculpt") || name.contains("texture") {
                                        blend_files.push(p);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if blend_files.is_empty() {
        return Err(
            "No Blender essentials brush .blend found under Program Files. \
             Use Import Tip… on a .blend, or Import Folder… on an alphas directory."
                .into(),
        );
    }

    let mut ok = 0usize;
    let mut err = 0usize;
    let mut first = None;
    for p in blend_files {
        match import_tips_from_path(&p) {
            Ok(list) => {
                ok += list.len();
                if first.is_none() {
                    first = list.into_iter().next();
                }
            }
            Err(_) => err += 1,
        }
    }
    if ok == 0 {
        // Stock Blender brushes are procedural — install SDF/noise essentials instead.
        let n = install_procedural_essentials_force()?;
        let first = list_library_entries()
            .into_iter()
            .next()
            .and_then(|e| load_library_tip(&e.name).map(|(t, p)| (e.name, p, t)));
        return Ok((n, err, first));
    }
    // Also ensure procedural pack is present alongside any imported images.
    let _ = ensure_procedural_essentials();
    Ok((ok, err, first))
}

// ═══════════════════════════════════════════════════════════════════════════
// Procedural Alpha Tool — SDF shapes + shader-node-style FBM noise → tip disc
// ═══════════════════════════════════════════════════════════════════════════

/// Procedural alpha generator kind (UI picker).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcAlphaKind {
    // ── SDF (production SdfShape::distance, XY slice) ─────────────────────
    SdfSphere,
    SdfBox,
    SdfBoxSoft, // rounded box via bevel
    SdfCylinder,
    SdfTorus,
    SdfCone,
    SdfCapsule,
    // ── Noise (matches shader Noise node fbm3 style) ──────────────────────
    FbmNoise,
    FbmRing, // FBM * annular mask
    // ── Classic discs ─────────────────────────────────────────────────────
    SoftRound,
    HardRound,
    ClayStrips,
    ThinLine,
    SquareSoft,
}

impl ProcAlphaKind {
    pub const SDF_ALL: [ProcAlphaKind; 7] = [
        Self::SdfSphere,
        Self::SdfBox,
        Self::SdfBoxSoft,
        Self::SdfCylinder,
        Self::SdfTorus,
        Self::SdfCone,
        Self::SdfCapsule,
    ];
    pub const ALL: [ProcAlphaKind; 14] = [
        Self::SdfSphere,
        Self::SdfBox,
        Self::SdfBoxSoft,
        Self::SdfCylinder,
        Self::SdfTorus,
        Self::SdfCone,
        Self::SdfCapsule,
        Self::FbmNoise,
        Self::FbmRing,
        Self::SoftRound,
        Self::HardRound,
        Self::ClayStrips,
        Self::ThinLine,
        Self::SquareSoft,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::SdfSphere => "SDF Sphere",
            Self::SdfBox => "SDF Box",
            Self::SdfBoxSoft => "SDF Round Box",
            Self::SdfCylinder => "SDF Cylinder",
            Self::SdfTorus => "SDF Torus",
            Self::SdfCone => "SDF Cone",
            Self::SdfCapsule => "SDF Capsule",
            Self::FbmNoise => "FBM Noise",
            Self::FbmRing => "FBM Ring",
            Self::SoftRound => "Soft Round",
            Self::HardRound => "Hard Round",
            Self::ClayStrips => "Clay Strips",
            Self::ThinLine => "Thin Line",
            Self::SquareSoft => "Square Soft",
        }
    }
    pub fn file_stem(self) -> &'static str {
        match self {
            Self::SdfSphere => "SDF Sphere",
            Self::SdfBox => "SDF Box",
            Self::SdfBoxSoft => "SDF Round Box",
            Self::SdfCylinder => "SDF Cylinder",
            Self::SdfTorus => "SDF Torus",
            Self::SdfCone => "SDF Cone",
            Self::SdfCapsule => "SDF Capsule",
            Self::FbmNoise => "FBM Noise",
            Self::FbmRing => "FBM Ring",
            Self::SoftRound => "Soft Round",
            Self::HardRound => "Hard Round",
            Self::ClayStrips => "Clay Strips",
            Self::ThinLine => "Thin Line",
            Self::SquareSoft => "Square Soft",
        }
    }
    pub fn is_sdf(self) -> bool {
        matches!(
            self,
            Self::SdfSphere
                | Self::SdfBox
                | Self::SdfBoxSoft
                | Self::SdfCylinder
                | Self::SdfTorus
                | Self::SdfCone
                | Self::SdfCapsule
        )
    }
    pub fn is_noise(self) -> bool {
        matches!(self, Self::FbmNoise | Self::FbmRing)
    }
}

/// Live procedural alpha tool parameters (sculpt panel).
#[derive(Clone, Debug)]
pub(crate) struct ProcAlphaParams {
    pub kind: ProcAlphaKind,
    /// Soft edge width in disc units (0.02–0.45). Higher = softer alpha rim.
    pub soft: f32,
    /// SDF size fill of the tip disc (0.4–0.95).
    pub fill: f32,
    /// Noise scale (shader node default ~4).
    pub scale: f32,
    /// FBM octaves 1–6 (shader node Noise).
    pub octaves: u32,
    pub seed: u32,
    /// Mid contrast after bake (1 = identity).
    pub contrast: f32,
    /// Invert alpha (black↔white).
    pub invert: bool,
    /// Hollow shell: use |d| band around surface (ring / outline tips).
    pub shell: bool,
    /// Shell half-width when `shell` (0.02–0.35).
    pub shell_w: f32,
}

impl Default for ProcAlphaParams {
    fn default() -> Self {
        Self {
            kind: ProcAlphaKind::SdfSphere,
            soft: 0.12,
            fill: 0.82,
            scale: 4.0,
            octaves: 4,
            seed: 42,
            contrast: 1.0,
            invert: false,
            shell: false,
            shell_w: 0.12,
        }
    }
}

/// Bake a greyscale tip from `ProcAlphaParams` (SDF / noise / classic).
pub(crate) fn generate_proc_alpha(p: &ProcAlphaParams) -> BrushTipAlpha {
    let res = TIP_RES;
    let soft = p.soft.clamp(0.01, 0.5);
    let fill = p.fill.clamp(0.3, 0.98);
    let mut tip = match p.kind {
        ProcAlphaKind::SoftRound => BrushTipAlpha::soft_round(res),
        ProcAlphaKind::HardRound => BrushTipAlpha::hard_round(res),
        ProcAlphaKind::ClayStrips => BrushTipAlpha::clay_strips(res),
        ProcAlphaKind::ThinLine => BrushTipAlpha::thin_line_crease(res, 0.055),
        ProcAlphaKind::SquareSoft => BrushTipAlpha::square_clay(res, soft),
        ProcAlphaKind::FbmNoise | ProcAlphaKind::FbmRing => {
            bake_noise_alpha(res, p)
        }
        k if k.is_sdf() => bake_sdf_alpha(res, p),
        _ => BrushTipAlpha::soft_round(res),
    };
    // Contrast + invert post
    let c = p.contrast.clamp(0.05, 4.0);
    for a in &mut tip.alpha {
        let mut v = *a;
        if (c - 1.0).abs() > 1e-4 {
            v = ((v - 0.5) * c + 0.5).clamp(0.0, 1.0);
        }
        if p.invert {
            v = 1.0 - v;
        }
        *a = v;
    }
    let _ = fill; // used inside bake_sdf
    tip
}

/// Save procedural tip into the library (unique name) and return path.
pub(crate) fn save_proc_alpha_to_library(
    name: &str,
    tip: &BrushTipAlpha,
) -> Result<(String, PathBuf), String> {
    let dir = tip_library_dir().ok_or_else(|| "No app data directory".to_string())?;
    let stem = unique_stem(&dir, if name.is_empty() { "Proc Alpha" } else { name });
    let path = write_tip_png(&dir, &stem, tip)?;
    Ok((stem, path))
}

fn bake_sdf_alpha(res: u32, p: &ProcAlphaParams) -> BrushTipAlpha {
    let fill = p.fill.clamp(0.3, 0.98);
    let soft = p.soft.clamp(0.01, 0.5);
    let shape = unit_sdf_shape(p.kind, fill);
    let shell = p.shell;
    let shell_w = p.shell_w.clamp(0.02, 0.4);
    BrushTipAlpha::from_fn(res, |u, v| {
        let r = (u * u + v * v).sqrt();
        if r > 1.0 {
            return 0.0;
        }
        // Tip disc = local XY plane (Z=0). Matches stamp plane UV.
        let pt = Vec3::new(u, v, 0.0);
        let d = shape.distance(pt);
        let a = if shell {
            // Band around surface: solid near d=0
            let ad = d.abs();
            if ad >= shell_w + soft {
                0.0
            } else if ad <= shell_w {
                1.0
            } else {
                let t = (ad - shell_w) / soft.max(1e-6);
                1.0 - t * t * (3.0 - 2.0 * t)
            }
        } else {
            // Solid interior: 1 inside, soft falloff outside
            // classic: smoothstep(soft, -soft, d) but d is SDF (neg inside for our convention?)
            // SdfShape: Sphere p.length()-radius → negative inside. Good.
            if d <= -soft {
                1.0
            } else if d >= soft {
                0.0
            } else {
                // d from -soft..+soft → 1..0
                let t = (d + soft) / (2.0 * soft).max(1e-6);
                1.0 - t * t * (3.0 - 2.0 * t)
            }
        };
        // Soft disc vignette
        let vignette = if r > 0.92 {
            let t = ((r - 0.92) / 0.08).clamp(0.0, 1.0);
            1.0 - t * t * (3.0 - 2.0 * t)
        } else {
            1.0
        };
        a * vignette
    })
}

fn unit_sdf_shape(kind: ProcAlphaKind, fill: f32) -> SdfShape {
    let f = fill;
    match kind {
        ProcAlphaKind::SdfSphere => SdfShape::Sphere { radius: f },
        ProcAlphaKind::SdfBox => SdfShape::Box {
            half_extents: Vec3::splat(f * 0.72),
            bevel: BoxBevel::default(),
        },
        ProcAlphaKind::SdfBoxSoft => {
            let he = Vec3::splat(f * 0.72);
            let mut bevel = BoxBevel::default();
            let r = f * 0.22;
            bevel.radii = [r; 12];
            bevel.modes = [core_math::sdf_shapes::BevelMode::Round; 12];
            SdfShape::Box {
                half_extents: he,
                bevel,
            }
        }
        ProcAlphaKind::SdfCylinder => SdfShape::Cylinder {
            radius: f * 0.85,
            half_height: 1.2, // tall so XY slice is a disc
        },
        ProcAlphaKind::SdfTorus => SdfShape::Torus {
            major_radius: f * 0.55,
            minor_radius: f * 0.28,
        },
        ProcAlphaKind::SdfCone => SdfShape::Cone {
            radius: f * 0.9,
            half_height: f * 0.9,
        },
        ProcAlphaKind::SdfCapsule => SdfShape::Capsule {
            radius: f * 0.45,
            half_height: f * 0.35,
        },
        _ => SdfShape::Sphere { radius: f },
    }
}

/// Shader-node-style FBM (matches `fbm3` in sdf_prim_eval.wgsl: value noise + octave sum).
fn bake_noise_alpha(res: u32, p: &ProcAlphaParams) -> BrushTipAlpha {
    let scale = p.scale.clamp(0.5, 32.0);
    let oct = p.octaves.clamp(1, 6);
    let seed = p.seed as f32 * 0.137;
    let ring = matches!(p.kind, ProcAlphaKind::FbmRing);
    let soft = p.soft.clamp(0.01, 0.5);
    BrushTipAlpha::from_fn(res, |u, v| {
        let r = (u * u + v * v).sqrt();
        if r > 1.0 {
            return 0.0;
        }
        let pos = Vec3::new(u * scale + seed, v * scale + seed * 1.3, seed * 0.7);
        let n = fbm3_cpu(pos, oct); // ~0..1-ish
        let body = n.clamp(0.0, 1.0);
        let disc = {
            let e = (1.0 - r).clamp(0.0, 1.0);
            // soft rim control
            let t = (e / soft.max(0.05)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        let a = if ring {
            // annular * noise
            let band = {
                let d = ((r - 0.55) / 0.35).abs().clamp(0.0, 1.0);
                1.0 - d * d * (3.0 - 2.0 * d)
            };
            body * band * disc
        } else {
            body * disc
        };
        a.clamp(0.0, 1.0)
    })
}

// ── CPU port of WGSL fbm3 / noise3 / hash31 (sdf_prim_eval.wgsl) ───────────

fn hash31_cpu(p: Vec3) -> f32 {
    let mut p3 = (p * 0.1031).fract_gl();
    p3 += p3.dot(Vec3::new(p3.y, p3.z, p3.x) + Vec3::splat(33.33));
    ((p3.x + p3.y) * p3.z).fract_gl()
}

fn noise3_cpu(p: Vec3) -> f32 {
    let i = p.floor();
    let f = p.fract_gl();
    let u = f * f * (Vec3::splat(3.0) - 2.0 * f);
    let a = hash31_cpu(i + Vec3::new(0.0, 0.0, 0.0));
    let b = hash31_cpu(i + Vec3::new(1.0, 0.0, 0.0));
    let c = hash31_cpu(i + Vec3::new(0.0, 1.0, 0.0));
    let d = hash31_cpu(i + Vec3::new(1.0, 1.0, 0.0));
    let e = hash31_cpu(i + Vec3::new(0.0, 0.0, 1.0));
    let f2 = hash31_cpu(i + Vec3::new(1.0, 0.0, 1.0));
    let g = hash31_cpu(i + Vec3::new(0.0, 1.0, 1.0));
    let h = hash31_cpu(i + Vec3::new(1.0, 1.0, 1.0));
    let x1 = a + (b - a) * u.x;
    let x2 = c + (d - c) * u.x;
    let y1 = x1 + (x2 - x1) * u.y;
    let x3 = e + (f2 - e) * u.x;
    let x4 = g + (h - g) * u.x;
    let y2 = x3 + (x4 - x3) * u.y;
    y1 + (y2 - y1) * u.z
}

fn fbm3_cpu(p: Vec3, octaves: u32) -> f32 {
    let mut sum = 0.0f32;
    let mut amp = 0.5f32;
    let mut pos = p;
    let max_oct = octaves.min(6);
    for _ in 0..max_oct {
        sum += amp * noise3_cpu(pos);
        pos *= 2.01;
        amp *= 0.5;
    }
    sum
}

trait FractGl {
    fn fract_gl(self) -> Self;
    fn floor(self) -> Self;
}
impl FractGl for f32 {
    fn fract_gl(self) -> Self {
        self - self.floor()
    }
    fn floor(self) -> Self {
        f32::floor(self)
    }
}
impl FractGl for Vec3 {
    fn fract_gl(self) -> Self {
        Vec3::new(self.x.fract_gl(), self.y.fract_gl(), self.z.fract_gl())
    }
    fn floor(self) -> Self {
        Vec3::new(self.x.floor(), self.y.floor(), self.z.floor())
    }
}

fn write_tip_png(dir: &Path, stem: &str, tip: &BrushTipAlpha) -> Result<PathBuf, String> {
    let path = dir.join(format!("{stem}.png"));
    let mut bytes = Vec::with_capacity(tip.alpha.len());
    for &a in &tip.alpha {
        bytes.push((a.clamp(0.0, 1.0) * 255.0) as u8);
    }
    let img = image::GrayImage::from_raw(tip.width, tip.height, bytes)
        .ok_or_else(|| "Invalid tip dimensions".to_string())?;
    img.save(&path).map_err(|e| e.to_string())?;
    invalidate_tip_library_list();
    Ok(path)
}

/// Load a library tip by basename (with or without extension).
pub(crate) fn load_library_tip(name: &str) -> Option<(BrushTipAlpha, PathBuf)> {
    let dir = tip_library_dir()?;
    let stem = sanitize_stem(
        name.trim_end_matches(".png")
            .trim_end_matches(".PNG")
            .trim_end_matches(".jpg")
            .trim_end_matches(".JPG"),
    );
    for ext in ["png", "jpg", "jpeg", "tga", "bmp", "webp"] {
        let p = dir.join(format!("{stem}.{ext}"));
        if p.exists() {
            let tip = decode_image_file(&p)?;
            return Some((tip, p));
        }
    }
    None
}

/// Greyscale tip → RGBA8 for egui preview (white form on dark bg, circular mask soft edge).
pub(crate) fn tip_preview_rgba(tip: &BrushTipAlpha, size: u32) -> (u32, u32, Vec<u8>) {
    let size = size.max(8);
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            // Sample tip UV in [-1,1] circle space (same as brush stamp tip)
            let u = (x as f32 + 0.5) / size as f32 * 2.0 - 1.0;
            let v = (y as f32 + 0.5) / size as f32 * 2.0 - 1.0;
            let r2 = u * u + v * v;
            let i = ((y * size + x) * 4) as usize;
            if r2 > 1.02 {
                // outside brush disc — dark void
                rgba[i] = 18;
                rgba[i + 1] = 18;
                rgba[i + 2] = 22;
                rgba[i + 3] = 255;
                continue;
            }
            let a = tip.sample(u, v).clamp(0.0, 1.0);
            // Soft rim
            let rim = if r2 > 0.92 {
                ((1.02 - r2) / 0.10).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let g = (a * rim * 255.0) as u8;
            // Slight warm clay tint so form reads on dark UI
            rgba[i] = g;
            rgba[i + 1] = (g as f32 * 0.95) as u8;
            rgba[i + 2] = (g as f32 * 0.88) as u8;
            rgba[i + 3] = 255;
        }
    }
    (size, size, rgba)
}

/// Cheap content fingerprint so previews refresh when tip pixels change (same name).
fn tip_content_fp(tip: &BrushTipAlpha) -> u64 {
    let mut h: u64 = (tip.width as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(tip.height as u64);
    // Sample a sparse grid of texels — enough to catch soft-edge / contrast / invert edits.
    let n = tip.alpha.len();
    if n == 0 {
        return h;
    }
    let step = (n / 64).max(1);
    let mut i = 0usize;
    while i < n {
        let bits = tip.alpha[i].to_bits() as u64;
        h ^= bits.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        h = h.rotate_left(13).wrapping_add(0x1656_67B1);
        i += step;
    }
    h
}

/// egui texture cache for tip library thumbnails (stem → handle).
pub(crate) struct BrushTipPreviewCache {
    thumbs: HashMap<String, egui::TextureHandle>,
    /// Active tip preview: (name, content fingerprint, texture).
    active: Option<(String, u64, egui::TextureHandle)>,
}

impl BrushTipPreviewCache {
    pub fn new() -> Self {
        Self {
            thumbs: HashMap::new(),
            active: None,
        }
    }

    /// 48² library thumb by name (loads PNG from disk once).
    pub fn thumb(
        &mut self,
        ctx: &egui::Context,
        name: &str,
    ) -> Option<egui::TextureId> {
        if let Some(h) = self.thumbs.get(name) {
            return Some(h.id());
        }
        let (tip, _) = load_library_tip(name)?;
        let (w, h, rgba) = tip_preview_rgba(&tip, TIP_THUMB_PX);
        let ci = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
        let handle = ctx.load_texture(
            format!("brush_tip_thumb_{name}"),
            ci,
            egui::TextureOptions::LINEAR,
        );
        let id = handle.id();
        self.thumbs.insert(name.to_string(), handle);
        Some(id)
    }

    /// Larger preview for the currently active tip (name + live alpha).
    /// Re-uploads when **pixels** change even if the display name stays the same
    /// (procedural soft-edge / contrast / invert, re-Apply, force-install Defaults).
    pub fn active_preview(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        tip: &BrushTipAlpha,
        size: u32,
    ) -> egui::TextureId {
        let fp = tip_content_fp(tip);
        if let Some((n, prev_fp, h)) = &self.active {
            if n == name && *prev_fp == fp {
                return h.id();
            }
        }
        let (w, h, rgba) = tip_preview_rgba(tip, size);
        let ci = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
        // Unique texture id each re-bake so egui does not keep a stale same-name image.
        let handle = ctx.load_texture(
            format!("brush_tip_active_{name}_{fp:x}"),
            ci,
            egui::TextureOptions::LINEAR,
        );
        let id = handle.id();
        self.active = Some((name.to_string(), fp, handle));
        id
    }

    /// Drop cached thumbs (e.g. after bulk import so new files appear).
    pub fn invalidate(&mut self) {
        self.thumbs.clear();
        self.active = None;
    }

    /// Cap cache size so bulk imports don't grow forever.
    pub fn trim(&mut self, max: usize) {
        if self.thumbs.len() > max {
            self.thumbs.clear();
        }
    }
}

// ─── Shared tip-picker UI ────────────────────────────────────────────────────
// ONE grid + ONE active-square implementation used by the Sculpt Brush panel AND
// the viewport-header Texture popovers (voxel + poly), so the pickers cannot
// drift. Every grid thumb is BOTH a click-picker and a drag source; the active
// square is the matching drop target (owner 2026-08-19: "clic and drag from
// library to thumbnail").

/// Drag-and-drop payload: a tip-library entry name dragged from any library grid.
#[derive(Clone)]
pub(crate) struct TipDragPayload(pub String);

/// Load a library entry — exact file path first, name lookup fallback. The ONE
/// loader every picker click / drop goes through.
pub(crate) fn load_entry(entry: &TipLibraryEntry) -> Option<(BrushTipAlpha, PathBuf)> {
    decode_tip_file(&entry.path)
        .map(|t| (t, entry.path.clone()))
        .or_else(|| load_library_tip(&entry.name))
}

/// Loader for drops (only the name crosses the dnd payload).
pub(crate) fn load_entry_by_name(name: &str) -> Option<(BrushTipAlpha, PathBuf)> {
    list_library_entries()
        .iter()
        .find(|e| e.name == name)
        .and_then(load_entry)
}

/// Scrollable thumbnail GRID of the tip library. Highlights `selected_name`; a click loads
/// and returns the picked `(name, path, tip)`; every thumb is also a [`TipDragPayload`]
/// drag source (drop onto an [`active_tip_square`]). `id_salt` keeps multiple live grids
/// (panel + popovers) from sharing scroll/drag ids.
pub(crate) fn tip_library_grid(
    ui: &mut egui::Ui,
    cache: &mut BrushTipPreviewCache,
    selected_name: &str,
    id_salt: &str,
) -> Option<(String, PathBuf, BrushTipAlpha)> {
    let tip_entries = list_library_entries();
    if tip_entries.is_empty() {
        ui.label(
            egui::RichText::new("No tips yet — Import / Folder / Blender")
                .small()
                .color(egui::Color32::from_rgb(120, 120, 130)),
        );
        return None;
    }
    let mut picked: Option<(String, PathBuf, BrushTipAlpha)> = None;
    ui.label(
        egui::RichText::new(format!("Library ({} tips)", tip_entries.len()))
            .small()
            .color(egui::Color32::from_rgb(150, 150, 160)),
    );
    let thumb = TIP_THUMB_PX as f32;
    let cell = thumb + 6.0;
    let avail_w = ui.available_width().max(cell);
    let cols = ((avail_w / cell).floor() as usize).max(1).min(8);
    egui::ScrollArea::vertical()
        .id_salt((id_salt, "tip_lib_scroll"))
        .max_height(if tip_entries.len() > cols * 2 { 150.0 } else { 110.0 })
        .show(ui, |ui| {
            egui::Grid::new((id_salt, "tip_lib_grid"))
                .num_columns(cols)
                .spacing([4.0, 4.0])
                .show(ui, |ui| {
                    for (i, entry) in tip_entries.iter().enumerate() {
                        let selected = selected_name == entry.name;
                        ui.vertical(|ui| {
                            ui.set_min_width(thumb);
                            let tex_id = cache.thumb(ui.ctx(), &entry.name);
                            let drag_id = egui::Id::new((id_salt, "tip_drag", &entry.name));
                            let clicked = ui
                                .dnd_drag_source(
                                    drag_id,
                                    TipDragPayload(entry.name.clone()),
                                    |ui| {
                                        if let Some(tid) = tex_id {
                                            let sized = egui::load::SizedTexture::new(
                                                tid,
                                                egui::vec2(thumb, thumb),
                                            );
                                            let mut img =
                                                egui::Image::new(sized).sense(egui::Sense::click());
                                            if selected {
                                                img = img.tint(egui::Color32::from_rgb(180, 220, 255));
                                            }
                                            ui.add(img).on_hover_text(&entry.name).clicked()
                                        } else {
                                            ui.add_sized(
                                                [thumb, thumb],
                                                egui::Button::new("?").frame(true),
                                            )
                                            .on_hover_text(&entry.name)
                                            .clicked()
                                        }
                                    },
                                )
                                .inner;
                            let short = if entry.name.len() > 10 {
                                format!("{}…", &entry.name[..9])
                            } else {
                                entry.name.clone()
                            };
                            let label = if selected {
                                egui::RichText::new(short)
                                    .small()
                                    .color(egui::Color32::from_rgb(160, 200, 255))
                            } else {
                                egui::RichText::new(short).small()
                            };
                            if (ui.selectable_label(selected, label).clicked() || clicked)
                                && picked.is_none()
                            {
                                if let Some((tip, path)) = load_entry(entry) {
                                    picked = Some((entry.name.clone(), path, tip));
                                }
                            }
                        });
                        if (i + 1) % cols == 0 {
                            ui.end_row();
                        }
                    }
                });
        });
    cache.trim(256);
    picked
}

/// The ACTIVE-TIP square: a `size`-px thumbnail of the brush's current tip (or a "＋"
/// placeholder), doubling as the drop target for tips dragged from any [`tip_library_grid`].
/// Returns `(clicked, dropped)` — `clicked` toggles the caller's picker; `dropped` is a
/// loaded `(name, path, tip)` from a completed drag.
pub(crate) fn active_tip_square(
    ui: &mut egui::Ui,
    cache: &mut BrushTipPreviewCache,
    tip: Option<&BrushTipAlpha>,
    tip_name: &str,
    size: f32,
) -> (bool, Option<(String, PathBuf, BrushTipAlpha)>) {
    let frame = egui::Frame::none()
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_gray(90)))
        .rounding(4.0)
        .inner_margin(2.0);
    let (inner, payload) = ui.dnd_drop_zone::<TipDragPayload, _>(frame, |ui| match tip {
        Some(t) => {
            let name = if tip_name.is_empty() { "active" } else { tip_name };
            let tex = cache.active_preview(ui.ctx(), name, t, size as u32);
            let sized = egui::load::SizedTexture::new(tex, egui::vec2(size, size));
            ui.add(egui::Image::new(sized).sense(egui::Sense::click()))
                .on_hover_text(format!(
                    "Brush tip: {name} ({}×{}) — white = full deposit, black = none. \
                     Click for the tip library, or drag a library thumb here.",
                    t.width, t.height
                ))
        }
        None => ui
            .add_sized(
                [size, size],
                egui::Button::new(egui::RichText::new("＋").size(20.0)),
            )
            .on_hover_text(
                "No tip — radial falloff only. Click for the tip library, \
                 or drag a library thumb here.",
            ),
    });
    let clicked = inner.inner.clicked();
    let dropped = payload
        .and_then(|p| load_entry_by_name(&p.0).map(|(t, path)| (p.0.clone(), path, t)));
    (clicked, dropped)
}

// ─── Blender .blend packed image extract ─────────────────────────────────────
// Modern Blender packs are zstd-compressed; older may be gzip or raw "BLENDER".
// We decompress then carve PNG/JPEG byte streams — works for essentials brush assets
// and any .blend with packed alpha textures (no full DNA parser needed).

fn decompress_blend(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 12 {
        return Err("blend file too small".into());
    }
    // Raw
    if data.starts_with(b"BLENDER") {
        return Ok(data.to_vec());
    }
    // gzip
    if data[0] == 0x1f && data[1] == 0x8b {
        return gzip_decompress(data).ok_or_else(|| "gzip decompress failed".into());
    }
    // zstd magic 28 B5 2F FD
    if data[0] == 0x28 && data[1] == 0xB5 && data[2] == 0x2F && data[3] == 0xFD {
        return zstd_decompress(data).ok_or_else(|| "zstd decompress failed".into());
    }
    // Some exporters leave a thin wrapper — try zstd anyway, then gzip.
    if let Some(v) = zstd_decompress(data) {
        return Ok(v);
    }
    if let Some(v) = gzip_decompress(data) {
        return Ok(v);
    }
    Err("Unrecognized .blend compression (need BLENDER / gzip / zstd)".into())
}

fn zstd_decompress(data: &[u8]) -> Option<Vec<u8>> {
    // Blender writes multi-frame zstd (magic can appear several times). Decode every frame.
    let mut out = Vec::new();
    let mut pos = 0usize;
    let magic: &[u8] = &[0x28, 0xB5, 0x2F, 0xFD];
    let mut buf = [0u8; 64 * 1024];
    while pos + 4 <= data.len() {
        // Find next zstd frame magic
        if &data[pos..pos + 4] != magic {
            // Scan forward a bit (padding between frames is rare but allowed)
            if let Some(rel) = find_bytes(&data[pos..], magic) {
                pos += rel;
            } else {
                break;
            }
        }
        let mut cursor = Cursor::new(&data[pos..]);
        let mut decoder = ruzstd::decoding::StreamingDecoder::new(&mut cursor).ok()?;
        loop {
            match decoder.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    out.extend_from_slice(&buf[..n]);
                    if out.len() > 256 * 1024 * 1024 {
                        return None;
                    }
                }
                Err(_) => return None,
            }
        }
        // Advance past the frame bytes the decoder consumed
        let consumed = cursor.position() as usize;
        if consumed == 0 {
            break;
        }
        pos += consumed;
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn gzip_decompress(_data: &[u8]) -> Option<Vec<u8>> {
    // Older .blend used gzip; modern Blender (4.x/5.x) uses zstd (handled above).
    // No flate2 dep yet — gzip packs can be re-saved in Blender or imported as exported PNGs.
    None
}

/// Extract greyscale tips from packed PNG/JPEG streams inside a `.blend`.
fn extract_blend_images(path: &Path) -> Result<Vec<(String, BrushTipAlpha)>, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let raw = decompress_blend(&data)?;
    let mut out = Vec::new();
    // PNG carve
    let png_sig = b"\x89PNG\r\n\x1a\n";
    let mut pos = 0usize;
    let mut png_i = 0usize;
    while pos + 8 < raw.len() && out.len() < 128 {
        if let Some(rel) = find_bytes(&raw[pos..], png_sig) {
            let start = pos + rel;
            if let Some(end) = find_png_iend(&raw[start..]) {
                let slice = &raw[start..start + end];
                if let Some(tip) = decode_image_bytes(slice) {
                    // Skip tiny icons (< 16² after decode still ok if tip has form)
                    if tip.width >= 8 && tip.height >= 8 {
                        png_i += 1;
                        out.push((format!("png{png_i:03}"), tip));
                    }
                }
                pos = start + end;
                continue;
            }
            pos = start + 8;
        } else {
            break;
        }
    }
    // JPEG carve
    let mut pos = 0usize;
    let mut jpg_i = 0usize;
    while pos + 4 < raw.len() && out.len() < 128 {
        if let Some(rel) = find_bytes(&raw[pos..], b"\xff\xd8\xff") {
            let start = pos + rel;
            if let Some(end) = find_jpeg_eoi(&raw[start..]) {
                let slice = &raw[start..start + end];
                if slice.len() > 256 {
                    if let Some(tip) = decode_image_bytes(slice) {
                        if tip.width >= 8 && tip.height >= 8 {
                            jpg_i += 1;
                            out.push((format!("jpg{jpg_i:03}"), tip));
                        }
                    }
                }
                pos = start + end;
                continue;
            }
            pos = start + 3;
        } else {
            break;
        }
    }
    // Dedup by crude content hash (identical packed previews)
    let mut seen = std::collections::HashSet::new();
    out.retain(|(_, tip)| {
        let mut h = 0u64;
        for (i, &a) in tip.alpha.iter().enumerate().step_by(7) {
            h = h.wrapping_mul(131).wrapping_add((a * 255.0) as u64).wrapping_add(i as u64);
        }
        h = h.wrapping_mul(31).wrapping_add(tip.width as u64);
        seen.insert(h)
    });
    Ok(out)
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len())
        .position(|w| w == needle)
}

fn find_png_iend(data: &[u8]) -> Option<usize> {
    // Walk PNG chunks until IEND
    if data.len() < 8 || &data[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut off = 8usize;
    for _ in 0..256 {
        if off + 12 > data.len() {
            return None;
        }
        let len = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) as usize;
        let ctype = &data[off + 4..off + 8];
        let next = off.checked_add(12 + len)?;
        if next > data.len() {
            return None;
        }
        if ctype == b"IEND" {
            return Some(next);
        }
        off = next;
    }
    None
}

fn find_jpeg_eoi(data: &[u8]) -> Option<usize> {
    // Scan for FF D9 (EOI). Cap scan length.
    let max = data.len().min(16 * 1024 * 1024);
    let mut i = 2usize;
    while i + 1 < max {
        if data[i] == 0xFF && data[i + 1] == 0xD9 {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

// ─── Photoshop ABR (sampled brushes) ─────────────────────────────────────────
// Big-endian layout aligned with GIMP's abr loader (v1/v2 and v6/v10 "samp" section).

fn be_u16(r: &mut Cursor<&[u8]>) -> Option<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b).ok()?;
    Some(u16::from_be_bytes(b))
}
fn be_i16(r: &mut Cursor<&[u8]>) -> Option<i16> {
    Some(be_u16(r)? as i16)
}
fn be_u32(r: &mut Cursor<&[u8]>) -> Option<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).ok()?;
    Some(u32::from_be_bytes(b))
}
fn be_i32(r: &mut Cursor<&[u8]>) -> Option<i32> {
    Some(be_u32(r)? as i32)
}
fn skip(r: &mut Cursor<&[u8]>, n: u64) -> Option<()> {
    let pos = r.position().saturating_add(n);
    if pos > r.get_ref().len() as u64 {
        return None;
    }
    r.set_position(pos);
    Some(())
}

fn decode_abr_largest_sample(path: &Path) -> Option<BrushTipAlpha> {
    let mut all = decode_abr_all_samples(path)?;
    all.sort_by_key(|t| std::cmp::Reverse(t.width * t.height));
    all.into_iter().next()
}

fn decode_abr_all_samples(path: &Path) -> Option<Vec<BrushTipAlpha>> {
    let data = std::fs::read(path).ok()?;
    if data.len() < 4 {
        return None;
    }
    let mut r = Cursor::new(data.as_slice());
    let version = be_u16(&mut r)?;
    let count_or_sub = be_u16(&mut r)?;
    match version {
        1 | 2 => decode_abr_v12(&mut r, version, count_or_sub as usize),
        6 | 10 if count_or_sub == 1 || count_or_sub == 2 => decode_abr_v6(&mut r),
        _ => None,
    }
}

fn decode_abr_v12(r: &mut Cursor<&[u8]>, version: u16, count: usize) -> Option<Vec<BrushTipAlpha>> {
    let mut out = Vec::new();
    for _ in 0..count.min(64) {
        let btype = be_i16(r)?;
        let size = be_i32(r)?;
        if size < 0 {
            break;
        }
        let start = r.position();
        let end = start + size as u64;
        if btype == 2 {
            if let Some(tip) = parse_abr_v12_sampled(r, version) {
                out.push(tip);
            }
        }
        r.set_position(end.min(r.get_ref().len() as u64));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn parse_abr_v12_sampled(r: &mut Cursor<&[u8]>, version: u16) -> Option<BrushTipAlpha> {
    let _misc = be_i32(r)?;
    let _spacing = be_i16(r)?;
    if version == 2 {
        let n = be_u32(r)? as usize;
        skip(r, (n.saturating_mul(2)) as u64)?;
    }
    let _aa = {
        let mut b = [0u8; 1];
        r.read_exact(&mut b).ok()?;
        b[0]
    };
    for _ in 0..4 {
        let _ = be_i16(r)?;
    }
    let mut bounds = [0i32; 4];
    for b in &mut bounds {
        *b = be_i32(r)?;
    }
    let depth = be_i16(r)?;
    let height = (bounds[2] - bounds[0]).max(0) as u32;
    let width = (bounds[3] - bounds[1]).max(0) as u32;
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return None;
    }
    let bytes = (depth / 8).max(1) as u32;
    if bytes != 1 {
        return None;
    }
    let mut compress = [0u8; 1];
    r.read_exact(&mut compress).ok()?;
    let size = (width * height * bytes) as usize;
    let mut mask = vec![0u8; size];
    if compress[0] == 0 {
        r.read_exact(&mut mask).ok()?;
    } else {
        abr_rle_decode(r, &mut mask, height as usize)?;
    }
    tip_from_raw(width, height, &mask)
}

fn decode_abr_v6(r: &mut Cursor<&[u8]>) -> Option<Vec<BrushTipAlpha>> {
    loop {
        let mut tag = [0u8; 4];
        if r.read_exact(&mut tag).is_err() {
            return None;
        }
        if &tag != b"8BIM" {
            return None;
        }
        let mut name = [0u8; 4];
        r.read_exact(&mut name).ok()?;
        if &name == b"samp" {
            break;
        }
        let section_size = be_i32(r)? as u64;
        skip(r, section_size)?;
    }
    let sample_section_size = be_i32(r)? as u64;
    let sample_end = r.position() + sample_section_size;
    let mut out = Vec::new();
    while r.position() < sample_end && out.len() < 64 {
        let brush_size = be_i32(r)?;
        if brush_size < 0 {
            break;
        }
        let mut brush_end = brush_size as u64;
        while brush_end % 4 != 0 {
            brush_end += 1;
        }
        let next = r.position() + brush_end;
        let try_offsets = [47u64, 301];
        let saved = r.position();
        let mut got = None;
        for off in try_offsets {
            r.set_position(saved);
            if skip(r, off).is_none() {
                continue;
            }
            if let Some(tip) = parse_abr_v6_sample_body(r) {
                got = Some(tip);
                break;
            }
        }
        if let Some(tip) = got {
            out.push(tip);
        }
        r.set_position(next.min(r.get_ref().len() as u64));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn parse_abr_v6_sample_body(r: &mut Cursor<&[u8]>) -> Option<BrushTipAlpha> {
    let top = be_i32(r)?;
    let left = be_i32(r)?;
    let bottom = be_i32(r)?;
    let right = be_i32(r)?;
    let depth = be_i16(r)?;
    let mut compress = [0u8; 1];
    r.read_exact(&mut compress).ok()?;
    let depth_bytes = (depth / 8).max(1) as i32;
    let width = (right - left).max(0) as u32;
    let height = (bottom - top).max(0) as u32;
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return None;
    }
    if depth_bytes != 1 {
        return None;
    }
    let size = (width * height) as usize;
    let mut mask = vec![0u8; size];
    if compress[0] == 0 {
        r.read_exact(&mut mask).ok()?;
    } else if compress[0] == 1 {
        abr_rle_decode(r, &mut mask, height as usize)?;
    } else {
        return None;
    }
    tip_from_raw(width, height, &mask)
}

fn abr_rle_decode(r: &mut Cursor<&[u8]>, out: &mut [u8], height: usize) -> Option<()> {
    if height == 0 || out.is_empty() {
        return Some(());
    }
    let width = out.len() / height;
    if width * height != out.len() {
        return None;
    }
    let mut lens = Vec::with_capacity(height);
    for _ in 0..height {
        lens.push(be_i16(r)? as i32);
    }
    for (row, &clen) in lens.iter().enumerate() {
        if clen < 0 {
            return None;
        }
        let mut row_buf = vec![0u8; clen as usize];
        r.read_exact(&mut row_buf).ok()?;
        let dst = &mut out[row * width..(row + 1) * width];
        unpack_packbits(&row_buf, dst)?;
    }
    Some(())
}

fn unpack_packbits(src: &[u8], dst: &mut [u8]) -> Option<()> {
    let mut si = 0usize;
    let mut di = 0usize;
    while di < dst.len() && si < src.len() {
        let n = src[si] as i8;
        si += 1;
        if n >= 0 {
            let count = n as usize + 1;
            if si + count > src.len() || di + count > dst.len() {
                return None;
            }
            dst[di..di + count].copy_from_slice(&src[si..si + count]);
            si += count;
            di += count;
        } else if n != -128 {
            let count = (-n as usize) + 1;
            if si >= src.len() || di + count > dst.len() {
                return None;
            }
            let b = src[si];
            si += 1;
            for _ in 0..count {
                dst[di] = b;
                di += 1;
            }
        }
    }
    Some(())
}

fn tip_from_raw(width: u32, height: u32, mask: &[u8]) -> Option<BrushTipAlpha> {
    let img = image::GrayImage::from_raw(width, height, mask.to_vec())?;
    let dynimg = image::DynamicImage::ImageLuma8(img);
    let img = dynimg.resize(TIP_RES, TIP_RES, image::imageops::FilterType::Lanczos3);
    let gray = img.to_luma8();
    let (w, h) = gray.dimensions();
    BrushTipAlpha::from_luma_u8(w, h, gray.as_raw())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tip_preview_rgba_has_expected_len() {
        let tip = BrushTipAlpha::from_luma_u8(4, 4, &[0, 64, 128, 255, 0, 64, 128, 255, 0, 64, 128, 255, 0, 64, 128, 255])
            .expect("tip");
        let (w, h, rgba) = tip_preview_rgba(&tip, 32);
        assert_eq!(w, 32);
        assert_eq!(h, 32);
        assert_eq!(rgba.len(), 32 * 32 * 4);
    }

    #[test]
    fn zstd_multiframe_decompresses_blender_blend() {
        let path = Path::new(
            r"C:\Program Files\Blender Foundation\Blender 4.5\4.5\datafiles\assets\brushes\essentials_brushes-mesh_sculpt.blend",
        );
        if !path.exists() {
            eprintln!("skip: Blender 4.5 sculpt blend not installed");
            return;
        }
        let data = std::fs::read(path).unwrap();
        let raw = zstd_decompress(&data).expect("zstd multi-frame");
        assert!(raw.starts_with(b"BLENDER"), "header {:?}", &raw[..raw.len().min(12)]);
        // Stock essentials are ~12MB DNA with no PNG streams (procedural brushes).
        assert!(raw.len() > 1_000_000, "expected multi-frame full decompress, got {}", raw.len());
        eprintln!("blender sculpt essentials decomp {} bytes, PNG carve tips={}",
            raw.len(), extract_blend_images(path).map(|t| t.len()).unwrap_or(0));
    }

    #[test]
    fn extract_png_bytes_from_synthetic_blend_payload() {
        // Minimal "blend-like" payload: BLENDER header + a tiny greyscale PNG.
        let mut png = Vec::new();
        {
            // 2×2 white PNG via image crate
            let img = image::GrayImage::from_pixel(2, 2, image::Luma([255u8]));
            img.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
        }
        let mut raw = b"BLENDER-v400REND".to_vec();
        raw.extend_from_slice(&png);
        // Compress single-frame zstd
        // We don't have a zstd encoder dep — call extract on raw by writing temp uncompressed path.
        // Test PNG carve path directly via decode of the synthetic buffer:
        let tips = {
            let mut out = Vec::new();
            let png_sig = b"\x89PNG\r\n\x1a\n";
            if let Some(rel) = find_bytes(&raw, png_sig) {
                if let Some(end) = find_png_iend(&raw[rel..]) {
                    if let Some(tip) = decode_image_bytes(&raw[rel..rel + end]) {
                        out.push(tip);
                    }
                }
            }
            out
        };
        assert_eq!(tips.len(), 1);
        assert!(tips[0].width >= 2);
    }
}
