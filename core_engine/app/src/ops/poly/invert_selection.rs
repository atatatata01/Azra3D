//! Op: invert poly component selection (Vertex/Edge/Face only).

use crate::{FourthLayerApp, ModelingContext, PolyWorkMode};

pub fn run(app: &mut FourthLayerApp) {
    if app.modeling_context == ModelingContext::Polygonal
        && app.poly_work_mode != PolyWorkMode::Object
    {
        app.poly_invert_selection();
    }
}
