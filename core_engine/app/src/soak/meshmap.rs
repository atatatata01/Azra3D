//! SOAK: production-API mesh-map bake (G0/G1 verify).

use super::scenarios::StepOutcome;
use crate::mesh_map_bake::MeshMapKind;
use crate::{AzraApp, SceneItemId};

fn select_poly(app: &mut AzraApp) -> Result<core_math::scene_graph::NodeId, StepOutcome> {
    let Some(nid) = app.soak.poly_nid else {
        return Err(StepOutcome::fail("no poly nid"));
    };
    app.modeling_context = crate::ModelingContext::Polygonal;
    app.poly_work_mode = crate::PolyWorkMode::Object;
    app.select_single_unified(SceneItemId::Node(nid));
    Ok(nid)
}

pub(super) fn step_poly_uv_unwrap(app: &mut AzraApp) -> StepOutcome {
    let nid = match select_poly(app) {
        Ok(n) => n,
        Err(e) => return e,
    };
    app.poly_auto_uv_unwrap();
    let has = app
        .cache
        .poly_custom_data
        .get(&nid)
        .is_some_and(|cd| !cd.uv.is_empty());
    if !has {
        return StepOutcome::fail("UV Unwrap produced no UVs");
    }
    StepOutcome::note(format!("uv corners={}", app.cache.poly_custom_data[&nid].uv.len()))
}

/// Queue the same path as both bake buttons (`poly_request_mesh_map_bake`).
pub(super) fn step_poly_bake_mesh_maps(app: &mut AzraApp) -> StepOutcome {
    let nid = match select_poly(app) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let mut s = app.bake_settings_for(nid);
    s.res_global = 64;
    s.enabled_maps = MeshMapKind::Position.bit();
    s.padding_px = 2;
    s.supersample = 1;
    app.store_bake_settings(nid, s);
    app.poly_request_mesh_map_bake();
    StepOutcome::note("queued Position@64")
}

/// After settle: registry reconnected, slot honours res_global.
pub(super) fn step_poly_bake_mesh_maps_check(app: &mut AzraApp) -> StepOutcome {
    let nid = match select_poly(app) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let Some(img) = app
        .ui
        .baked_images
        .by_body_kind(nid, MeshMapKind::Position.bit())
        .cloned()
    else {
        return StepOutcome::fail("no Position in registry after settle");
    };
    if img.res != 64 {
        return StepOutcome::fail(format!("slot res {} != 64", img.res));
    }
    if img.pixels_missing || img.slot == 0 {
        return StepOutcome::fail("pixels missing / slot 0");
    }
    if app.baked_mesh_map(nid, MeshMapKind::Position).is_none() {
        return StepOutcome::fail("baked_mesh_map missed the registry");
    }
    StepOutcome::note(format!("{} slot={}", img.name, img.slot))
}
