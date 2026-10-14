//! Op: select linked poly elements from hover/selection.

use crate::{FourthLayerApp, ModelingContext};

pub fn run(app: &mut FourthLayerApp) {
    if app.modeling_context == ModelingContext::Polygonal {
        app.poly_select_linked_hover(false);
    }
}
