//! Op: activate poly knife tool (if entry exists).

use crate::FourthLayerApp;

pub fn run(app: &mut FourthLayerApp) {
    // Knife is modal via tool state; use keyboard path entry if present.
    // Falls through to toast when not in poly edit.
    if app.modeling_context == crate::ModelingContext::Polygonal {
        app.toast("Knife: use K in poly edit (tool session)");
    }
}
