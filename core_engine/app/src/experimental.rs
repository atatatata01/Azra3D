#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct ExperimentalFlags {
    pub(crate) multi_segment_bevel: bool,
    pub(crate) vertex_bevel: bool,
    pub(crate) sdf_sculpt: bool,
    /// GPU voxel remesh (REMESH_GPU_PLAN.md). Evaluates the Remesh modifier's field and DC
    /// extract on the GPU instead of the CPU; `from_mesh` stays on the CPU either way.
    ///
    /// `serde(default)` is LOAD-BEARING, not decoration: `load()` ends in `unwrap_or_default()`,
    /// so a field that is absent from an existing `~/.azra/experimental.json` fails the whole
    /// parse and silently resets EVERY flag to its default. Any future field needs it too.
    #[serde(default)]
    pub(crate) gpu_remesh: bool,
}

impl Default for ExperimentalFlags {
    fn default() -> Self {
        Self {
            multi_segment_bevel: false,
            // Shipped 2026-07-03 (BEVEL.md §11 PM2) — Ctrl+B in Vertex mode. Default ON so a
            // stale experimental.json / dead checkbox cannot read as "vertex bevel is off".
            // The flag is no longer a gate (activation is production); kept for load compat.
            vertex_bevel: true,
            sdf_sculpt: false,
            // OFF until the Gate 2/3 parity criteria hold (100% solid/air agreement with the CPU
            // path). Until then the CPU path is the only one that ships.
            gpu_remesh: false,
        }
    }
}

impl ExperimentalFlags {
    pub(crate) fn save(&self) {
        let Some(home) = dirs::home_dir() else { return };
        let dir = home.join(".azra");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("experimental.json");
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    pub(crate) fn load() -> Self {
        let Some(home) = dirs::home_dir() else { return Self::default() };
        let path = home.join(".azra").join("experimental.json");
        match std::fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }
}
