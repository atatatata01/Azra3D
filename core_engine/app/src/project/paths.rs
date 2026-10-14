//! Well-known paths + recent files + library scan. PURE RELOCATION from project.rs.
use super::*;

pub(crate) fn auto_save_path(project_path: &Option<PathBuf>) -> PathBuf {
    if let Some(p) = project_path {
        let mut bak = p.clone();
        bak.set_extension("fl4.bak");
        bak
    } else {
        // Fallback: ~/.azra/autosave.fl4.bak
        let dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".azra");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("autosave.fl4.bak")
    }
}

fn recent_files_path() -> PathBuf {
    let dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".azra");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("recent_files.json")
}

pub(crate) fn load_recent_files() -> Vec<PathBuf> {
    let path = recent_files_path();
    let Ok(data) = std::fs::read_to_string(&path) else { return vec![]; };
    serde_json::from_str(&data).unwrap_or_default()
}

pub(crate) fn save_recent_files(files: &[PathBuf]) {
    let path = recent_files_path();
    let _ = atomic_write(&path, serde_json::to_string(files).unwrap_or_default().as_bytes());
}

// ==================================================================
// Default Scene
// ==================================================================

/// Fixed path for the user's default startup scene.
pub(crate) fn default_scene_path() -> PathBuf {
    let dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Azra");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("default_scene.fl4")
}

// ==================================================================
// Body Library (.flb)
// ==================================================================

/// A serializable entry for the SDF body library.
/// Contains the body's scene node (with prims) and its child subtree.
#[derive(Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub(crate) struct BodyLibraryEntry {
    pub version: u32,
    pub name: String,
    pub node: core_math::scene_graph::SceneNode,
    /// Child body subtrees (recursive). Empty for leaf bodies.
    pub children: Vec<BodyLibraryEntry>,
}

/// Returns the library directory, creating it if needed.
/// `%LOCALAPPDATA%/Azra/library/` on Windows.
#[allow(dead_code)]
pub(crate) fn library_path() -> PathBuf {
    let base = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Azra")
        .join("library");
    let _ = std::fs::create_dir_all(&base);
    base
}

#[allow(dead_code)]
pub(crate) fn load_library_entry(path: &Path) -> Result<BodyLibraryEntry, String> {
    let data = std::fs::read(path).map_err(|e| format!("Read {}: {}", path.display(), e))?;
    if data.is_empty() {
        return Err(format!(
            "Empty library file (0 bytes): {}\nRe-save the part from the scene (right-click → Save to Library).",
            path.display()
        ));
    }
    // Deserialize on a thread with 8MB stack (same stack overflow risk as serialize)
    let data_for_thread = data.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::Builder::new()
        .name("lib-deserialize".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let _ = tx.send(bincode::deserialize::<BodyLibraryEntry>(&data_for_thread));
        })
        .map_err(|e| format!("Spawn deserialize thread: {}", e))?;
    handle.join().map_err(|_| "Deserialize thread panicked".to_string())?;
    let result = rx
        .recv()
        .map_err(|_| "Deserialize channel closed".to_string())?;
    match result {
        Ok(entry) => Ok(entry),
        Err(e) => {
            // Recovery: poly parts saved with an older/shorter PolyModifier wire fail at the
            // modifiers field (EOF). Mesh + material still load — drop modifiers and keep the part.
            if let Some(recovered) = recover_poly_library_entry(&data) {
                eprintln!(
                    "[Library] recovered poly part from outdated modifiers: {}",
                    path.display()
                );
                return Ok(recovered);
            }
            let detail = e.to_string();
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            if detail.contains("end of file")
                || detail.contains("io error")
                || detail.contains("unexpected")
            {
                Err(format!(
                    "\"{stem}\" is corrupt or outdated ({detail}).\n\
                     Re-save from the scene (right-click body → Save to Library), or delete the bad .flb in %LOCALAPPDATA%/Azra/library/."
                ))
            } else {
                Err(format!("\"{stem}\": {detail}"))
            }
        }
    }
}

/// Recover a poly `.flb` whose `PolyModifier` stack is an older shorter wire image.
///
/// Full `BodyLibraryEntry` deserialize fails at `modifiers` (EOF). Everything up to and
/// including `material` is still valid — rebuild the node with an empty modifier stack and
/// default transform so the part can still be placed (interactive place sets position).
fn recover_poly_library_entry(data: &[u8]) -> Option<BodyLibraryEntry> {
    use core_math::half_edge::HalfEdgeMesh;
    use core_math::primitives::PbrMaterial;
    use core_math::scene_graph::{MeshCacheId, NodeId, NodePayload, PivotConfig, SceneNode};
    use std::io::Cursor;
    use std::sync::Arc;

    let mut c = Cursor::new(data);
    let version: u32 = bincode::deserialize_from(&mut c).ok()?;
    let name: String = bincode::deserialize_from(&mut c).ok()?;
    let id: u64 = bincode::deserialize_from(&mut c).ok()?;
    let parent: Option<u64> = bincode::deserialize_from(&mut c).ok()?;
    let children: Vec<u64> = bincode::deserialize_from(&mut c).ok()?;
    // NodePayload tag: PolygonalBody == 6 (append-only enum — see payload.rs).
    let payload_tag: u32 = bincode::deserialize_from(&mut c).ok()?;
    if payload_tag != 6 {
        return None;
    }
    let pname: String = bincode::deserialize_from(&mut c).ok()?;
    let mesh: HalfEdgeMesh = bincode::deserialize_from(&mut c).ok()?;
    let mesh_cache_id: Option<u64> = bincode::deserialize_from(&mut c).ok()?;
    let material: PbrMaterial = bincode::deserialize_from(&mut c).ok()?;
    // Intentionally skip broken modifiers + trailing transform — place session repositions.
    let _ = (parent, children);
    let node = SceneNode {
        id: NodeId(id),
        parent: None,
        children: Vec::new(),
        payload: NodePayload::PolygonalBody {
            name: if pname.is_empty() {
                name.clone()
            } else {
                pname
            },
            mesh: Arc::new(mesh),
            source: core_math::primitive_spec::MeshSource::Authored,
            mesh_cache_id: mesh_cache_id.map(MeshCacheId),
            material,
            modifiers: Vec::new(),
        },
        local_position: glam::Vec3::ZERO,
        local_rotation: glam::Vec3::ZERO,
        local_scale: glam::Vec3::ONE,
        visible: true,
        locked: false,
        pivot: PivotConfig::default(),
    };
    // SceneNode fields are public but construction may need to go through graph — check if
    // SceneNode is constructible. If `new` is pub(super) only, build via deserialize of a
    // minimal synthetic entry instead.
    let _ = version;
    // Prefer building a complete entry via bincode of a known-good structure when SceneNode
    // fields aren't all pub — they are pub on SceneNode.
    Some(BodyLibraryEntry {
        version: 1,
        name,
        node,
        children: Vec::new(),
    })
}

#[allow(dead_code)]
pub(crate) fn scan_library() -> Vec<(String, PathBuf)> {
    let dir = library_path();
    let Ok(entries) = std::fs::read_dir(&dir) else { return vec![]; };
    let mut result = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().map(|x| x == "flb").unwrap_or(false) {
            let name = p.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            result.push((name, p));
        }
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

// ==================================================================
// Save/Load round-trip tests — child_order backward-compat (P0 data safety)
//
// These guard the regression that the adversarial verifier caught: a `child_order` trailing field
// inside the bincode blob corrupted every v7 file. The fix: child_order is #[serde(skip)] (blob
// byte-identical to v7) and travels in an optional FL4ORDER side-channel appended after the blob.
// ==================================================================
