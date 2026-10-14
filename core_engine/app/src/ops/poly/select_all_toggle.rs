//! Op: poly select-all toggle (edit mode) or visible unlocked prims (object).

use crate::{FourthLayerApp, ModelingContext, PolyWorkMode, SceneItemId};

pub fn run(app: &mut FourthLayerApp) {
    if app.modeling_context == ModelingContext::Polygonal
        && app.poly_work_mode != PolyWorkMode::Object
    {
        app.poly_select_all_toggle();
    } else {
        for p in &app.scene.primitives {
            if p.visible && !p.locked {
                app.sel.usel.selected.insert(SceneItemId::Prim(p.prim_uid));
            }
        }
    }
}
