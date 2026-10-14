//! Op: Mesh Cleanup — repair folded flaps / degenerate faces and re-merge coplanar
//! n-gons on the active poly body (heals bodies poisoned by pre-fix boolean builds).

use crate::FourthLayerApp;

pub fn run(app: &mut FourthLayerApp) {
    app.poly_mesh_cleanup();
}
