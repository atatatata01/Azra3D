//! Ops: Blender **Mesh > Clean Up** commands, one pass each.
//!
//! ONE file rather than the usual one-file-per-op, because these are a family of identical
//! four-line façades over the same entry point — seven files whose only difference is which field
//! of [`CleanupOptions`] is non-default would drift the moment one of them grew a special case.
//!
//! Every command runs through [`AzraApp::poly_mesh_cleanup_apply`], the SAME install path the
//! Mesh Cleanup dialog uses, so each one inherits its gates (never installs a mesh that is
//! invalid, more open, or more folded than before), its transaction, its sidecar carry, and its
//! result report. The only thing that varies is which pass is switched on — and, critically,
//! `Default::default()` leaves `flatten_folds` / `merge_coplanar` ON, so a "single pass" here is
//! genuinely a cleanup run with that pass added, exactly as Blender's Clean Up entries behave
//! against a mesh that also needs the basics.

use crate::FourthLayerApp;
use core_math::mesh_boolean::CleanupOptions;

/// Resolve the target body + its protected (authored) edges, then run `opts`.
/// Refuses with a toast — and no undo entry — when there is no polygonal body to work on.
fn run_with(app: &mut FourthLayerApp, label: &str, mut opts: CleanupOptions) {
    let Some(nid) = app.poly_cleanup_target() else {
        app.toast(&format!("{label}: select a polygonal body first"));
        return;
    };
    let Some(mesh) = app.poly_node_mesh(nid) else { return };
    opts.protected_edges = app.poly_protected_edge_positions(nid, &mesh);
    app.poly_mesh_cleanup_apply(nid, opts, false);
}

/// Blender **Merge by Distance** over the WHOLE body (the `Merge Vertices` command is the
/// selection-scoped one). Uses Blender's own 1e-4 default.
pub fn merge_by_distance(app: &mut FourthLayerApp) {
    run_with(
        app,
        "Merge by Distance",
        CleanupOptions { merge_distance: 1e-4, ..Default::default() },
    );
}

/// Blender **Degenerate Dissolve**: collapse edges shorter than 1e-4 and drop the zero-area
/// faces they leave.
pub fn degenerate_dissolve(app: &mut FourthLayerApp) {
    run_with(
        app,
        "Degenerate Dissolve",
        CleanupOptions { degenerate_threshold: 1e-4, ..Default::default() },
    );
}

/// Blender **Delete Loose**: unreferenced verts + wire edges. Isolated FACES are excluded — on a
/// sheet or decal mesh an isolated face is the model, so removing it is a modelling decision;
/// the dialog exposes that switch for when it is wanted.
pub fn delete_loose(app: &mut FourthLayerApp) {
    run_with(
        app,
        "Delete Loose",
        CleanupOptions {
            delete_loose_verts: true,
            delete_loose_edges: true,
            ..Default::default()
        },
    );
}

/// Blender **Fill Holes** at its default 4-side limit.
pub fn fill_holes(app: &mut FourthLayerApp) {
    run_with(
        app,
        "Fill Holes",
        CleanupOptions { fill_holes_sides: 4, ..Default::default() },
    );
}

/// Blender **Make Planar Faces** (default: 1 iteration at factor 1.0).
pub fn make_planar_faces(app: &mut FourthLayerApp) {
    run_with(
        app,
        "Make Planar Faces",
        CleanupOptions { make_planar_iterations: 1, ..Default::default() },
    );
}

/// Blender **Split Non-Planar Faces** at its default 5° limit.
pub fn split_non_planar_faces(app: &mut FourthLayerApp) {
    run_with(
        app,
        "Split Non-Planar Faces",
        CleanupOptions { split_non_planar_deg: 5.0, ..Default::default() },
    );
}

/// Blender **Split Concave Faces**.
pub fn split_concave_faces(app: &mut FourthLayerApp) {
    run_with(
        app,
        "Split Concave Faces",
        CleanupOptions { split_concave: true, ..Default::default() },
    );
}
