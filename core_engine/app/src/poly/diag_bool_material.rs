//! APP-PATH verification that a poly BOOLEAN keeps per-face materials (owner 2026-08-07:
//! "doing a boolean, it should pick up the material").
//!
//! Project rule `feedback_verify_app_path_not_kernel`: a green kernel test proves nothing about
//! what the user sees. These tests build a headless `AzraApp` with two poly bodies carrying
//! DISTINCT materials, drive the REAL X-pie entry (`poly_boolean_execute`), and read the
//! materials back off the mesh INSTALLED in the scene graph — then flip the redo panel's cutter
//! policy and read them again.
//!
//! Exact ids are asserted. "Not zero" would pass a fix that floods one material over the whole
//! body, which destroys the user's assignment just as thoroughly.
//!
//! Run: `cargo test -p app poly::diag_bool_material -- --nocapture`

#[cfg(test)]
mod tests {
    use crate::poly::bool_material::BoolCutterMaterial;
    use crate::{AzraApp, SceneItemId};
    use core_math::half_edge::{FaceId, HalfEdgeMesh, NO_EDGE};
    use core_math::mesh_boolean::BoolOp;
    use core_math::scene_graph::{NodeId, NodePayload};

    fn add_body(app: &mut AzraApp, name: &str, mut mesh: HalfEdgeMesh, mat: u32) -> NodeId {
        for f in mesh.faces.iter_mut() {
            f.material_id = mat;
        }
        let root = app.scene.scene_graph.root();
        app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: name.into(),
                mesh: std::sync::Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        )
    }

    fn installed_materials(app: &AzraApp, nid: NodeId) -> Vec<u32> {
        let m = app.poly_node_mesh(nid).expect("poly body still present");
        (0..m.faces.len() as u32)
            .map(FaceId)
            .filter(|f| m.face(*f).half_edge != NO_EDGE)
            .map(|f| m.face(f).material_id)
            .collect()
    }

    /// Cube (material 3) minus a through-cylinder (material 7), driven through the real
    /// object-mode boolean: ACTIVE = target, other selected = cutter.
    /// Returns (app, target node).
    fn cube_minus_cylinder(mode: BoolCutterMaterial) -> (AzraApp, NodeId) {
        let mut app = AzraApp::new_headless();
        app.modeling_context = crate::ModelingContext::Polygonal;
        app.ui.bool_cutter_material = mode;
        let target = add_body(&mut app, "Target", HalfEdgeMesh::new_box(glam::Vec3::splat(2.0)), 3);
        // Taller than the cube so it punches clean through — no cap coplanarity to argue about.
        let cutter =
            add_body(&mut app, "Cutter", HalfEdgeMesh::new_cylinder(0.5, 6.0, 16), 7);
        // Cutter first, target second → target is active (the survivor that keeps the result).
        app.select_single_unified(SceneItemId::Node(cutter));
        app.toggle_select_unified(SceneItemId::Node(target));
        assert_eq!(
            app.sel.usel.active_node(),
            Some(target),
            "the second-selected body must be active (= the boolean target)"
        );

        app.poly_boolean_execute(BoolOp::Difference); // ← the real X-pie entry
        (app, target)
    }

    #[test]
    fn app_boolean_keeps_target_material_and_takes_cutter_material_by_default() {
        let (app, target) = cube_minus_cylinder(BoolCutterMaterial::Cutter);
        let mats = installed_materials(&app, target);
        assert!(!mats.is_empty(), "the boolean must have installed a result");
        assert!(
            !mats.contains(&0),
            "no result face may fall back to the default material (got {mats:?})"
        );
        assert!(mats.contains(&3), "the cube's own walls must keep material 3 (got {mats:?})");
        assert!(
            mats.contains(&7),
            "the hole's barrel must carry the cutter's material 7 by default (got {mats:?})"
        );
    }

    #[test]
    fn app_boolean_cutter_material_target_mode_gives_one_material() {
        let (app, target) = cube_minus_cylinder(BoolCutterMaterial::Target);
        let mats = installed_materials(&app, target);
        assert!(!mats.is_empty(), "the boolean must have installed a result");
        assert!(
            mats.iter().all(|&m| m == 3),
            "Target mode: every face reads as the target's material 3 (got {mats:?})"
        );
    }

    #[test]
    fn app_boolean_cutter_material_none_mode_strips_only_the_cutter_faces() {
        let (app, target) = cube_minus_cylinder(BoolCutterMaterial::None);
        let mats = installed_materials(&app, target);
        assert!(mats.contains(&3), "the target's own walls keep material 3 (got {mats:?})");
        assert!(
            mats.contains(&0),
            "None mode: the carved faces fall back to the default material (got {mats:?})"
        );
        assert!(
            mats.iter().all(|&m| m == 0 || m == 3),
            "the cutter's material 7 must be gone entirely (got {mats:?})"
        );
    }

    /// The owner's actual ask: the choice must remain adjustable AFTER the destructive op.
    /// The redo panel re-stamps the stored plan — no geometry is recomputed.
    #[test]
    fn app_boolean_material_stays_adjustable_after_the_destructive_op() {
        let (mut app, target) = cube_minus_cylinder(BoolCutterMaterial::Cutter);
        let before = installed_materials(&app, target);
        assert!(before.contains(&7), "precondition: cutter material is on the result");
        assert!(
            app.ui.poly_boolean_adjust.is_some(),
            "the boolean must open its adjust panel"
        );
        let faces_before = app.poly_node_mesh(target).unwrap().faces.len();

        // Flip the policy the way the panel does.
        app.poly_boolean_apply_material_mode_for_test(BoolCutterMaterial::None);
        let after = installed_materials(&app, target);
        assert!(
            !after.contains(&7),
            "after switching to 'No material' the cutter id must be gone (got {after:?})"
        );
        assert!(after.contains(&3), "the target's own material survives the switch");
        assert_eq!(
            app.poly_node_mesh(target).unwrap().faces.len(),
            faces_before,
            "the re-stamp must not touch geometry — face count is unchanged"
        );

        // And back again: the plan keeps BOTH candidates, so it is reversible.
        app.poly_boolean_apply_material_mode_for_test(BoolCutterMaterial::Cutter);
        assert_eq!(
            installed_materials(&app, target),
            before,
            "switching back must restore exactly the original assignment"
        );
    }
}
