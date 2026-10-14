//! Op: hide selected poly elements (edit mode).

use crate::{FourthLayerApp, ModelingContext, PolyWorkMode};

pub fn run(app: &mut FourthLayerApp) {
    if app.modeling_context == ModelingContext::Polygonal
        && app.poly_work_mode != PolyWorkMode::Object
    {
        app.poly_hide_selected();
    }
}
