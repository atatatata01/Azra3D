//! Op: group selected SDF prims into a body (layer).

use crate::{FourthLayerApp, ModelingContext};

pub fn run(app: &mut FourthLayerApp) {
    if app.modeling_context == ModelingContext::Sdf {
        app.group_selected_into_body(None);
    } else {
        app.toast("Group/layer is for SDF body selection");
    }
}
