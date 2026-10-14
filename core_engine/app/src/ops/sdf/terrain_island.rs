//! Op: create Mesh Terrain island (ADR-TERRAIN-001).
//!
//! Shift+A → SDF → **Terrain** only creates an island. Patch size, stream, expand,
//! rebuild, and export live in **Tool Properties** when a terrain tile is selected
//! (`AzraApp::draw_tool_params_terrain`). Demo tunnel is tests-only (not in UI).

use crate::FourthLayerApp;

/// Shift+A: create one Terrain Island (single tile). Further tiles via Tool Properties.
pub fn run(app: &mut FourthLayerApp) {
    app.create_terrain_island();
}
