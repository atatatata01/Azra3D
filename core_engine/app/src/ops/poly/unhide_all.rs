//! Op: unhide all poly elements.

use crate::{FourthLayerApp, ModelingContext};

pub fn run(app: &mut FourthLayerApp) {
    if app.modeling_context == ModelingContext::Polygonal {
        app.poly_unhide_all();
    }
}
