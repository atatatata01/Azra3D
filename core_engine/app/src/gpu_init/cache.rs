//! GPU pipeline cache — path, validated load, atomic save.
//! Moved from main.rs. The envelope (header / version / size / checksum) and
//! the temp+sync+rename writer live in `graphics::pipeline_cache_store`.

use eframe::wgpu;
use std::sync::Mutex;

use graphics::pipeline_cache_store;

/// Path for the on-disk GPU pipeline cache.
///
/// Windows: `%LOCALAPPDATA%\Azra\pipeline_cache.bin`
/// (stable across Rust rebuilds; `LOCALAPPDATA` is the per-user directory).
/// If `LOCALAPPDATA` is unset: `<exe_dir>/Azra/pipeline_cache.bin`, else
/// `<temp>/Azra/pipeline_cache.bin`.
pub(crate) fn pipeline_cache_path() -> std::path::PathBuf {
    let dir = std::env::var("LOCALAPPDATA")
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .unwrap_or_else(std::env::temp_dir)
        });
    let cache_dir = dir.join("Azra");
    std::fs::create_dir_all(&cache_dir).ok();
    cache_dir.join("pipeline_cache.bin")
}

/// Last successful save fingerprint: `(len, cheap_hash)` of the wgpu payload
/// (not the envelope). Saves can run from the main thread and the background
/// pipeline compiler — the mutex covers the compare-and-write (CONSOLE_AUDIT B2).
static LAST_SAVED: Mutex<Option<(usize, u64)>> = Mutex::new(None);

/// Load pipeline cache data from disk.
///
/// Returns `None` when the file is missing, unreadable, or rejected
/// (header, version, size, checksum). A rejection logs and the caller
/// compiles pipelines cold.
pub(crate) fn load_pipeline_cache_data() -> Option<Vec<u8>> {
    let path = pipeline_cache_path();
    let bytes = match std::fs::read(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "[CACHE] No cache file at {:?} — first launch, cold compile",
                path
            );
            return None;
        }
        Err(e) => {
            eprintln!(
                "[CACHE] rejected {:?}: read error ({e}) — cold compile",
                path
            );
            return None;
        }
    };
    match pipeline_cache_store::validate(&bytes) {
        Ok(payload) => {
            eprintln!("[CACHE] Loaded {} bytes from {:?}", payload.len(), path);
            if let Ok(mut g) = LAST_SAVED.lock() {
                *g = Some(pipeline_cache_store::digest(payload));
            }
            Some(payload.to_vec())
        }
        Err(reason) => {
            eprintln!("[CACHE] rejected {:?}: {reason} — cold compile", path);
            None
        }
    }
}

/// Save pipeline cache data to disk.
///
/// Skips the driver read and the write when called during unwind. Identical
/// payloads are skipped (CONSOLE_AUDIT B2). A real write is a temp file in the
/// same directory, flush + sync, then rename — never a torn `pipeline_cache.bin`.
pub(crate) fn save_pipeline_cache(cache: &wgpu::PipelineCache) {
    if std::thread::panicking() {
        eprintln!("[CACHE] panic unwind — skip pipeline cache write");
        return;
    }
    let Some(data) = cache.get_data() else {
        eprintln!("[CACHE] get_data() returned None — backend may not support caching");
        return;
    };
    if data.is_empty() {
        eprintln!("[CACHE] get_data() returned an empty blob — skip");
        return;
    }
    let dig = pipeline_cache_store::digest(&data);
    let mut guard = match LAST_SAVED.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.as_ref() == Some(&dig) {
        eprintln!("[CACHE] unchanged — skip ({} bytes)", dig.0);
        return;
    }
    let path = pipeline_cache_path();
    match pipeline_cache_store::write_atomic(&path, &data) {
        Ok(()) => {
            *guard = Some(dig);
            eprintln!("[CACHE] Saved {} bytes to {:?}", data.len(), path);
        }
        Err(e) => {
            eprintln!("[CACHE] WRITE FAILED: {e} — path {:?}", path);
        }
    }
}
