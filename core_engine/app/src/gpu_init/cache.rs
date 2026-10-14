//! GPU pipeline cache + texture server — moved from main.rs.

use eframe::wgpu;
use std::sync::Mutex;


/// Path for the on-disk GPU pipeline cache.
/// Uses %LOCALAPPDATA%\Azra\ (stable across Rust rebuilds).
pub(crate) fn pipeline_cache_path() -> std::path::PathBuf {
    let dir = std::env::var("LOCALAPPDATA").ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_exe().ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| std::env::temp_dir()));
    let cache_dir = dir.join("Azra");
    std::fs::create_dir_all(&cache_dir).ok();
    cache_dir.join("pipeline_cache.bin")
}

/// Last successful save fingerprint: `(len, cheap_hash)`. Saves can run from the main
/// thread and the background pipeline compiler — Mutex is enough (CONSOLE_AUDIT B2).
static LAST_SAVED: Mutex<Option<(usize, u64)>> = Mutex::new(None);

/// Cheap non-crypto digest over the whole buffer (FNV-1a 64-bit style).
fn cache_digest(data: &[u8]) -> (usize, u64) {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    // Mix length so equal-prefix different-size buffers never collide on hash alone.
    h ^= data.len() as u64;
    h = h.wrapping_mul(0x0100_0000_01b3);
    (data.len(), h)
}


/// Load pipeline cache data from disk (returns None if not found or corrupt).
pub(crate) fn load_pipeline_cache_data() -> Option<Vec<u8>> {
    let path = pipeline_cache_path();
    match std::fs::read(&path) {
        Ok(data) => {
            eprintln!("[CACHE] Loaded {} bytes from {:?}", data.len(), path);
            // Seed skip-fingerprint so the first post-load save doesn't rewrite identical bytes.
            if let Ok(mut g) = LAST_SAVED.lock() {
                *g = Some(cache_digest(&data));
            }
            Some(data)
        }
        Err(_) => {
            eprintln!("[CACHE] No cache file at {:?} — first launch", path);
            None
        }
    }
}


/// Save pipeline cache data to disk.
/// CONSOLE_AUDIT B2: skip when byte-identical to the last successful write (warm launches
/// used to rewrite ~37 MB × 7 times per start). Atomic tmp+rename is unchanged.
pub(crate) fn save_pipeline_cache(cache: &wgpu::PipelineCache) {
    if let Some(data) = cache.get_data() {
        let dig = cache_digest(&data);
        if let Ok(g) = LAST_SAVED.lock() {
            if g.as_ref() == Some(&dig) {
                eprintln!(
                    "[CACHE] unchanged — skip ({} bytes)",
                    dig.0
                );
                return;
            }
        }
        let path = pipeline_cache_path();
        // ATOMIC write: write to a temp file, then rename over the real one. A direct fs::write that is
        // interrupted (crash / abort mid-write) leaves a TRUNCATED pipeline_cache.bin, which makes the
        // NEXT launch hang in GPU pipeline creation (the relaunch-hang that cost a whole session). rename
        // is atomic on the same filesystem, so the real cache is always either the old valid file or the
        // complete new one — never half-written.
        let tmp = path.with_extension("tmp");
        let result = std::fs::write(&tmp, &data).and_then(|_| std::fs::rename(&tmp, &path));
        match result {
            Ok(_) => {
                if let Ok(mut g) = LAST_SAVED.lock() {
                    *g = Some(dig);
                }
                eprintln!("[CACHE] Saved {} bytes to {:?}", data.len(), path);
            }
            Err(e) => {
                eprintln!("[CACHE] WRITE FAILED: {} — path {:?}", e, path);
                let _ = std::fs::remove_file(&tmp); // drop any partial temp
            }
        }
    } else {
        eprintln!("[CACHE] get_data() returned None — backend may not support caching");
    }
}
