//! User brush preset library — Save As / Load for voxel sculpt tool state.
//! Stored under `%LOCALAPPDATA%/Azra/brushes/*.brush.json` (versioned JSON).
//! TOOL state only — not scene undo / bincode.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// On-disk brush preset (v1). Tip image reloads from `tip_path` when present.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct UserBrushPreset {
    #[serde(default = "preset_version")]
    pub version: u16,
    pub name: String,
    /// `VoxelBrush` debug name, e.g. "Draw", "ClayBuildup".
    pub brush: String,
    pub radius: f32,
    pub strength: f32,
    pub falloff: f32,
    #[serde(default)]
    pub falloff_preset: u32,
    pub stroke_spacing: f32,
    pub lazy: f32,
    /// `BrushDirection` as u32 (Surface=0 … AxisZ=5) — see `dir_to_u32`.
    pub brush_dir: u32,
    /// Add (true) / Remove (false). Ctrl flips temporarily. Default true.
    #[serde(default = "default_true")]
    pub brush_add: bool,
    /// Average Normal memory length (0.25..=4). Default 1.
    #[serde(default = "default_one")]
    pub dir_average_region: f32,
    /// Blend resolved direction toward average normal (0..=1). Default 0.
    #[serde(default)]
    pub dir_smooth_normal: f32,
    pub pinch_intensity: f32,
    #[serde(default)]
    pub tip_path: Option<String>,
    #[serde(default)]
    pub tip_name: String,
    #[serde(default)]
    pub auto_smooth: bool,
    #[serde(default = "default_auto_smooth_i")]
    pub auto_smooth_intensity: f32,
    #[serde(default)]
    pub surface_extrude: bool,
    /// Sculpt on Surface (ZBrush/Blender). Default true.
    #[serde(default = "default_true")]
    pub sculpt_on_surface: bool,
    #[serde(default = "default_layer_h")]
    pub layer_height: f32,
    #[serde(default = "default_layer_g")]
    pub layer_gain: f32,
    #[serde(default = "default_plane_off")]
    pub plane_offset: f32,
    #[serde(default)]
    pub plane_bias: f32,
    #[serde(default = "default_build_rate")]
    pub build_rate: f32,
    #[serde(default = "default_height_lim")]
    pub height_limit: f32,
    #[serde(default = "default_flat_amt")]
    pub flatten_amount: f32,
    #[serde(default)]
    pub volume_preservation: f32,
    #[serde(default = "default_true")]
    pub accumulate: bool,
    #[serde(default = "default_inflate_speed")]
    pub inflate_speed: f32,
    #[serde(default)]
    pub inflate_direction: u32,
    #[serde(default = "default_thick_clamp")]
    pub thickness_clamp: f32,
    #[serde(default = "default_one")]
    pub pinch_width: f32,
    #[serde(default = "default_one")]
    pub pinch_amount: f32,
    #[serde(default = "default_one")]
    pub crease_width: f32,
    #[serde(default = "default_one")]
    pub crease_depth: f32,
    #[serde(default)]
    pub plane_angle: f32,
    #[serde(default = "default_one")]
    pub cut_depth: f32,
    #[serde(default)]
    pub fill_holes: bool,
    // Brush Dynamics (default 0 = off for each)
    #[serde(default)]
    pub dyn_jitter: f32,
    #[serde(default)]
    pub dyn_scatter: f32,
    #[serde(default)]
    pub dyn_random_rotation: f32,
    #[serde(default)]
    pub dyn_random_strength: f32,
    #[serde(default)]
    pub dyn_random_radius: f32,
    #[serde(default)]
    pub dyn_stroke_noise: f32,
    #[serde(default = "default_pressure_rad_amt")]
    pub pressure_radius_amount: f32,
    #[serde(default)]
    pub tip_follow_stroke: bool,
    /// TipOrientation as u32 (Fixed=0, Follow=1, View=2, Random=3).
    #[serde(default)]
    pub tip_orient: u32,
    /// Alpha angle degrees 0..360.
    #[serde(default)]
    pub tip_angle: f32,
    #[serde(default = "default_tip_contrast")]
    pub tip_contrast: f32,
    #[serde(default)]
    pub front_faces_only: bool,
    #[serde(default = "default_one")]
    pub move_elasticity: f32,
    #[serde(default = "default_one")]
    pub move_pull_strength: f32,
    #[serde(default)]
    pub move_connected_only: bool,
    /// `AutoMaskMode` as u32 (Off=0, Curv=1, Peak=2, Cavity=3, Flat=4).
    #[serde(default)]
    pub auto_mask: u32,
    #[serde(default = "default_auto_mask_i")]
    pub auto_mask_intensity: f32,
    #[serde(default)]
    pub auto_mask_invert: bool,
    #[serde(default)]
    pub stack_gather: bool,
    #[serde(default)]
    pub stack_flatten: bool,
    #[serde(default)]
    pub stack_fill: bool,
    #[serde(default)]
    pub stack_crease: bool,
    #[serde(default)]
    pub stack_inflate: bool,
    #[serde(default)]
    pub symmetry: [bool; 3],
}

fn default_layer_h() -> f32 { 0.14 }
fn default_layer_g() -> f32 { 0.22 }
fn default_plane_off() -> f32 { 0.10 }
fn default_build_rate() -> f32 { 2.0 }
fn default_height_lim() -> f32 { 1.0 }
fn default_flat_amt() -> f32 { 0.5 }
fn default_true() -> bool { true }
fn default_inflate_speed() -> f32 { 1.0 }
fn default_thick_clamp() -> f32 { 1.0 }
fn default_one() -> f32 { 1.0 }
fn default_pressure_rad_amt() -> f32 { 0.60 }
fn default_tip_contrast() -> f32 { 1.0 }

fn preset_version() -> u16 { 1 }
fn default_auto_smooth_i() -> f32 { 0.35 }
fn default_auto_mask_i() -> f32 { 0.75 }

impl UserBrushPreset {
    pub(crate) fn from_voxel(v: &crate::VoxelObject, name: &str) -> Self {
        let s = v.capture_brush_settings();
        Self {
            version: 1,
            name: name.to_string(),
            brush: format!("{:?}", v.brush),
            radius: s.radius,
            strength: s.strength,
            falloff: s.falloff,
            falloff_preset: s.falloff_preset.as_u32(),
            stroke_spacing: s.stroke_spacing,
            lazy: s.lazy,
            brush_dir: dir_to_u32(s.brush_dir),
            brush_add: s.brush_add,
            dir_average_region: s.dir_average_region,
            dir_smooth_normal: s.dir_smooth_normal,
            pinch_intensity: s.pinch_intensity,
            tip_path: v.brush_tip_path.clone(),
            tip_name: v.brush_tip_name.clone(),
            auto_smooth: s.auto_smooth,
            auto_smooth_intensity: s.auto_smooth_intensity,
            surface_extrude: s.surface_extrude,
            sculpt_on_surface: s.sculpt_on_surface,
            layer_height: s.layer_height,
            layer_gain: s.layer_gain,
            plane_offset: s.plane_offset,
            plane_bias: s.plane_bias,
            build_rate: s.build_rate,
            height_limit: s.height_limit,
            flatten_amount: s.flatten_amount,
            volume_preservation: s.volume_preservation,
            accumulate: s.accumulate,
            inflate_speed: s.inflate_speed,
            inflate_direction: s.inflate_direction,
            thickness_clamp: s.thickness_clamp,
            pinch_width: s.pinch_width,
            pinch_amount: s.pinch_amount,
            crease_width: s.crease_width,
            crease_depth: s.crease_depth,
            plane_angle: s.plane_angle,
            cut_depth: s.cut_depth,
            fill_holes: s.fill_holes,
            dyn_jitter: s.dyn_jitter,
            dyn_scatter: s.dyn_scatter,
            dyn_random_rotation: s.dyn_random_rotation,
            dyn_random_strength: s.dyn_random_strength,
            dyn_random_radius: s.dyn_random_radius,
            dyn_stroke_noise: s.dyn_stroke_noise,
            pressure_radius_amount: s.pressure_radius_amount,
            tip_follow_stroke: s.tip_follow_stroke,
            tip_orient: s.tip_orient,
            tip_angle: s.tip_angle,
            tip_contrast: s.tip_contrast,
            front_faces_only: s.front_faces_only,
            move_elasticity: s.move_elasticity,
            move_pull_strength: s.move_pull_strength,
            move_connected_only: s.move_connected_only,
            auto_mask: s.auto_mask.as_u32(),
            auto_mask_intensity: s.auto_mask_intensity,
            auto_mask_invert: s.auto_mask_invert,
            stack_gather: s.stack_gather,
            stack_flatten: s.stack_flatten,
            stack_fill: s.stack_fill,
            stack_crease: s.stack_crease,
            stack_inflate: s.stack_inflate,
            symmetry: v.symmetry,
        }
    }

    pub(crate) fn apply_to(&self, v: &mut crate::VoxelObject) {
        if let Some(b) = brush_from_name(&self.brush) {
            v.brush = b;
        }
        v.apply_brush_settings(crate::VoxelBrushSettings {
            radius: self.radius.clamp(0.05, 1.0),
            strength: self.strength.clamp(0.05, 1.0),
            falloff: self.falloff.clamp(0.0, 1.0),
            falloff_preset: core_math::voxel_sculpt::FalloffPreset::from_u32(self.falloff_preset),
            brush_dir: dir_from_u32(self.brush_dir),
            brush_add: self.brush_add,
            dir_average_region: self.dir_average_region.clamp(0.25, 4.0),
            dir_smooth_normal: self.dir_smooth_normal.clamp(0.0, 1.0),
            stroke_spacing: self.stroke_spacing.clamp(0.03, 0.50),
            lazy: self.lazy.clamp(0.0, 1.0),
            pinch_intensity: self.pinch_intensity.max(0.0),
            auto_smooth: self.auto_smooth,
            auto_smooth_intensity: self.auto_smooth_intensity.clamp(0.0, 1.0),
            surface_extrude: self.surface_extrude,
            sculpt_on_surface: self.sculpt_on_surface,
            layer_height: self.layer_height.clamp(0.0, 0.5),
            layer_gain: self.layer_gain.clamp(0.0, 0.6),
            plane_offset: self.plane_offset.clamp(0.0, 0.5),
            plane_bias: self.plane_bias.clamp(-0.3, 0.3),
            build_rate: self.build_rate.clamp(0.0, 4.0),
            height_limit: self.height_limit.clamp(0.05, 1.0),
            flatten_amount: self.flatten_amount.clamp(0.0, 2.0),
            volume_preservation: self.volume_preservation.clamp(0.0, 1.0),
            accumulate: self.accumulate,
            inflate_speed: self.inflate_speed.clamp(0.0, 4.0),
            inflate_direction: self.inflate_direction.min(2),
            thickness_clamp: self.thickness_clamp.clamp(0.05, 1.0),
            pinch_width: self.pinch_width.clamp(0.05, 4.0),
            pinch_amount: self.pinch_amount.clamp(0.0, 2.0),
            crease_width: self.crease_width.clamp(0.1, 4.0),
            crease_depth: self.crease_depth.clamp(0.0, 4.0),
            plane_angle: self.plane_angle.clamp(-1.0, 1.0),
            cut_depth: self.cut_depth.clamp(0.0, 4.0),
            fill_holes: self.fill_holes,
            dyn_jitter: self.dyn_jitter.clamp(0.0, 1.0),
            dyn_scatter: self.dyn_scatter.clamp(0.0, 2.0),
            dyn_random_rotation: self.dyn_random_rotation.clamp(0.0, 1.0),
            dyn_random_strength: self.dyn_random_strength.clamp(0.0, 1.0),
            dyn_random_radius: self.dyn_random_radius.clamp(0.0, 1.0),
            dyn_stroke_noise: self.dyn_stroke_noise.clamp(0.0, 1.0),
            pressure_radius_amount: self.pressure_radius_amount.clamp(0.0, 1.0),
            tip_follow_stroke: self.tip_follow_stroke,
            tip_orient: self.tip_orient.min(3),
            tip_angle: self.tip_angle.rem_euclid(360.0),
            tip_contrast: self.tip_contrast.clamp(0.05, 4.0),
            front_faces_only: self.front_faces_only,
            move_elasticity: self.move_elasticity.clamp(0.0, 2.0),
            move_pull_strength: self.move_pull_strength.clamp(0.0, 2.0),
            move_connected_only: self.move_connected_only,
            auto_mask: core_math::voxel_sculpt::AutoMaskMode::from_u32(self.auto_mask),
            auto_mask_intensity: self.auto_mask_intensity.clamp(0.0, 1.0),
            auto_mask_invert: self.auto_mask_invert,
            stack_gather: self.stack_gather,
            stack_flatten: self.stack_flatten,
            stack_fill: self.stack_fill,
            stack_crease: self.stack_crease,
            stack_inflate: self.stack_inflate,
        });
        v.symmetry = self.symmetry;
        v.brush_tip_name = self.tip_name.clone();
        v.brush_tip_path = self.tip_path.clone();
        // Resolve tip: path first, then library name (procedural / factory alphas often have path=None).
        v.brush_tip = None;
        if let Some(ref path) = self.tip_path {
            if let Some(tip) = load_tip_from_path(Path::new(path)) {
                v.brush_tip = Some(tip);
            }
        }
        if v.brush_tip.is_none() && !self.tip_name.is_empty() {
            if let Some((tip, path)) = crate::brush_tip_lib::load_library_tip(&self.tip_name) {
                v.brush_tip = Some(tip);
                v.brush_tip_path = Some(path.to_string_lossy().to_string());
            }
        }
    }
}

fn load_tip_from_path(path: &Path) -> Option<core_math::voxel_sculpt::BrushTipAlpha> {
    crate::brush_tip_lib::decode_tip_file(path)
}

fn brush_from_name(s: &str) -> Option<core_math::voxel_sculpt::VoxelBrush> {
    use core_math::voxel_sculpt::VoxelBrush as B;
    Some(match s {
        "Draw" => B::Draw,
        "Erase" => B::Erase,
        "Smooth" => B::Smooth,
        "Flatten" => B::Flatten,
        "Clay" => B::Clay,
        "Scrape" => B::Scrape,
        "Crease" => B::Crease,
        "Fill" => B::Fill,
        "ClayBuildup" => B::ClayBuildup,
        "Inflate" => B::Inflate,
        "Pinch" => B::Pinch,
        "Move" => B::Move,
        "Smudge" => B::Smudge,
        _ => return None,
    })
}

fn dir_to_u32(d: core_math::voxel_sculpt::BrushDirection) -> u32 {
    d.as_u32()
}

fn dir_from_u32(v: u32) -> core_math::voxel_sculpt::BrushDirection {
    // Legacy Save As: old 0=Surface,1=View,2=First,3/4/5=Axis — map via from_u32 which
    // uses the new layout; old 2 still lands on FirstPoint via from_u32.
    core_math::voxel_sculpt::BrushDirection::from_u32(v)
}

/// `%LOCALAPPDATA%/Azra/brushes` (Windows) or platform equivalent.
pub(crate) fn brush_library_dir() -> Option<PathBuf> {
    let base = dirs::data_local_dir()?;
    let dir = base.join("Azra").join("brushes");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

/// Bump after save/delete so the sculpt brush menu refreshes without per-frame readdir.
static BRUSH_LIST_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub(crate) fn invalidate_brush_preset_list() {
    BRUSH_LIST_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn list_brush_presets() -> Vec<String> {
    use std::sync::{Mutex, OnceLock};
    struct Cache {
        gen: u64,
        names: Vec<String>,
    }
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(Cache { gen: 0, names: Vec::new() }));
    let gen = BRUSH_LIST_GEN.load(std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut g) = cache.lock() {
        if g.gen == gen {
            return g.names.clone();
        }
        let names = scan_brush_presets();
        g.gen = gen;
        g.names = names.clone();
        return names;
    }
    scan_brush_presets()
}

fn scan_brush_presets() -> Vec<String> {
    let Some(dir) = brush_library_dir() else { return Vec::new(); };
    let mut names = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return names; };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") { continue; }
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        // Accept `Name.brush.json` or `Name.json`
        let name = stem.strip_suffix(".brush").unwrap_or(stem);
        if !name.is_empty() { names.push(name.to_string()); }
    }
    names.sort();
    names
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' })
        .collect::<String>()
        .trim()
        .to_string()
}

pub(crate) fn save_brush_preset(preset: &UserBrushPreset) -> Result<PathBuf, String> {
    let dir = brush_library_dir().ok_or_else(|| "No app data dir".to_string())?;
    let safe = sanitize_filename(&preset.name);
    if safe.is_empty() { return Err("Empty brush name".into()); }
    let path = dir.join(format!("{safe}.brush.json"));
    let json = serde_json::to_string_pretty(preset).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    invalidate_brush_preset_list();
    Ok(path)
}

pub(crate) fn load_brush_preset(name: &str) -> Result<UserBrushPreset, String> {
    let dir = brush_library_dir().ok_or_else(|| "No app data dir".to_string())?;
    let safe = sanitize_filename(name);
    let path = dir.join(format!("{safe}.brush.json"));
    let path = if path.exists() {
        path
    } else {
        dir.join(format!("{safe}.json"))
    };
    let s = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_json::from_str(&s).map_err(|e| e.to_string())
}

pub(crate) fn delete_brush_preset(name: &str) -> Result<(), String> {
    let dir = brush_library_dir().ok_or_else(|| "No app data dir".to_string())?;
    let safe = sanitize_filename(name);
    let path = dir.join(format!("{safe}.brush.json"));
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
        invalidate_brush_preset_list();
        Ok(())
    } else {
        Err("Preset not found".into())
    }
}

/// Default brush-settings POD used to seed industry factories (radius kept user-scale ~0.12).
fn industry_base_settings() -> crate::VoxelBrushSettings {
    use core_math::voxel_sculpt::{AutoMaskMode, BrushDirection, FalloffPreset};
    crate::VoxelBrushSettings {
        radius: 0.12,
        strength: 0.40,
        falloff: 0.0,
        falloff_preset: FalloffPreset::Smooth,
        // Blender stroke model (2026-07-23): live per-dab surface normal by default.
        brush_dir: BrushDirection::Surface,
        brush_add: true,
        dir_average_region: 1.0,
        dir_smooth_normal: 0.0,
        stroke_spacing: 0.06,
        // Gate 0 (feel audit 2026-07-23): LazyMouse off by default — stabilization is opt-in.
        // "ZB Standard" keeps its explicit per-brush lazy (LazyMouse is part of that identity).
        lazy: 0.0,
        pinch_intensity: 1.0,
        auto_smooth: false,
        auto_smooth_intensity: 0.35,
        surface_extrude: false,
        sculpt_on_surface: true,
        layer_height: 0.14,
        layer_gain: 0.22,
        plane_offset: 0.10,
        plane_bias: 0.0,
        build_rate: 2.0,
        height_limit: 1.0,
        flatten_amount: 0.5,
        volume_preservation: 0.0,
        accumulate: true,
        inflate_speed: 1.0,
        inflate_direction: 0,
        thickness_clamp: 1.0,
        pinch_width: 1.0,
        pinch_amount: 1.0,
        crease_width: 1.0,
        crease_depth: 1.0,
        plane_angle: 0.0,
        cut_depth: 1.0,
        fill_holes: false,
        dyn_jitter: 0.0,
        dyn_scatter: 0.0,
        dyn_random_rotation: 0.0,
        dyn_random_strength: 0.0,
        dyn_random_radius: 0.0,
        dyn_stroke_noise: 0.0,
        auto_mask: AutoMaskMode::Off,
        auto_mask_intensity: 0.75,
        auto_mask_invert: false,
        stack_gather: false,
        stack_flatten: false,
        stack_fill: false,
        stack_crease: false,
        stack_inflate: false,
        pressure_radius_amount: 0.60,
        tip_follow_stroke: false,
        tip_orient: 0,
        tip_angle: 0.0,
        tip_contrast: 1.0,
        front_faces_only: false,
        move_elasticity: 1.0,
        move_pull_strength: 1.0,
        move_connected_only: false,
    }
}

fn preset_from_settings(
    name: &str,
    brush: core_math::voxel_sculpt::VoxelBrush,
    s: crate::VoxelBrushSettings,
) -> UserBrushPreset {
    UserBrushPreset {
        version: 1,
        name: name.to_string(),
        brush: format!("{:?}", brush),
        radius: s.radius,
        strength: s.strength,
        falloff: s.falloff,
        falloff_preset: s.falloff_preset.as_u32(),
        stroke_spacing: s.stroke_spacing,
        lazy: s.lazy,
        brush_dir: dir_to_u32(s.brush_dir),
        brush_add: s.brush_add,
        dir_average_region: s.dir_average_region,
        dir_smooth_normal: s.dir_smooth_normal,
        pinch_intensity: s.pinch_intensity,
        tip_path: None,
        tip_name: String::new(),
        auto_smooth: s.auto_smooth,
        auto_smooth_intensity: s.auto_smooth_intensity,
        surface_extrude: s.surface_extrude,
        sculpt_on_surface: s.sculpt_on_surface,
        layer_height: s.layer_height,
        layer_gain: s.layer_gain,
        plane_offset: s.plane_offset,
        plane_bias: s.plane_bias,
        build_rate: s.build_rate,
        height_limit: s.height_limit,
        flatten_amount: s.flatten_amount,
        volume_preservation: s.volume_preservation,
        accumulate: s.accumulate,
        inflate_speed: s.inflate_speed,
        inflate_direction: s.inflate_direction,
        thickness_clamp: s.thickness_clamp,
        pinch_width: s.pinch_width,
        pinch_amount: s.pinch_amount,
        crease_width: s.crease_width,
        crease_depth: s.crease_depth,
        plane_angle: s.plane_angle,
        cut_depth: s.cut_depth,
        fill_holes: s.fill_holes,
        dyn_jitter: s.dyn_jitter,
        dyn_scatter: s.dyn_scatter,
        dyn_random_rotation: s.dyn_random_rotation,
        dyn_random_strength: s.dyn_random_strength,
        dyn_random_radius: s.dyn_random_radius,
        dyn_stroke_noise: s.dyn_stroke_noise,
        pressure_radius_amount: s.pressure_radius_amount,
        tip_follow_stroke: s.tip_follow_stroke,
        tip_orient: s.tip_orient,
        tip_angle: s.tip_angle,
        tip_contrast: s.tip_contrast,
        front_faces_only: s.front_faces_only,
        move_elasticity: s.move_elasticity,
        move_pull_strength: s.move_pull_strength,
        move_connected_only: s.move_connected_only,
        auto_mask: s.auto_mask.as_u32(),
        auto_mask_intensity: s.auto_mask_intensity,
        auto_mask_invert: s.auto_mask_invert,
        stack_gather: s.stack_gather,
        stack_flatten: s.stack_flatten,
        stack_fill: s.stack_fill,
        stack_crease: s.stack_crease,
        stack_inflate: s.stack_inflate,
        symmetry: [false, false, false],
    }
}

/// Write ZBrush / 3D-Coat-oriented starter brushes into AppData brush menu.
/// Overwrites only these named slots. Returns how many were written.
pub(crate) fn install_industry_starters() -> Result<usize, String> {
    use core_math::voxel_sculpt::VoxelBrush;
    let base = industry_base_settings();
    let starters: [(&str, VoxelBrush); 6] = [
        ("ZB Clay BuildUp", VoxelBrush::ClayBuildup),
        ("ZB Clay", VoxelBrush::Clay),
        ("ZB DamStandard", VoxelBrush::Crease),
        ("ZB hPolish", VoxelBrush::Flatten),
        ("Coat Clay", VoxelBrush::Clay),
        ("ZB Standard", VoxelBrush::Draw),
    ];
    let mut n = 0usize;
    for (name, brush) in starters {
        let mut s = crate::VoxelObject::factory_settings_for(brush, base);
        // Named variants: Coat Clay = slightly softer AS + denser plane fill.
        if name == "Coat Clay" {
            s.auto_smooth = true;
            s.auto_smooth_intensity = 0.12;
            s.stroke_spacing = 0.04;
            s.strength = 0.40;
            s.plane_offset = 0.10;
            s.falloff = 0.50;
        }
        if name == "ZB Standard" {
            s.strength = 0.38;
            s.stroke_spacing = 0.06;
            s.falloff = 0.15;
            s.lazy = 0.05;
        }
        let p = preset_from_settings(name, brush, s);
        save_brush_preset(&p)?;
        n += 1;
    }
    Ok(n)
}

impl crate::AzraApp {
    pub(crate) fn brush_save_as(&mut self, name: &str) {
        let Some(v) = self.sculpt.voxel.as_ref() else {
            self.toast("No active voxel sculpt");
            return;
        };
        let preset = UserBrushPreset::from_voxel(v, name);
        match save_brush_preset(&preset) {
            Ok(p) => self.toast(&format!("Brush saved: {}", p.file_name().unwrap_or_default().to_string_lossy())),
            Err(e) => self.toast(&format!("Save brush failed: {e}")),
        }
    }

    pub(crate) fn brush_load_preset(&mut self, name: &str) {
        match load_brush_preset(name) {
            Ok(p) => {
                if let Some(v) = self.sculpt.voxel.as_mut() {
                    p.apply_to(v);
                    self.toast(&format!("Brush loaded: {}", p.name));
                } else {
                    self.toast("No active voxel sculpt");
                }
            }
            Err(e) => self.toast(&format!("Load brush failed: {e}")),
        }
    }
}
