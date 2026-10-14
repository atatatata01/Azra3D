//! Op: cycle create boolean op Union→Subtract→Intersect→Slice.

use crate::{CreateOp, FourthLayerApp};

pub fn run(app: &mut FourthLayerApp) {
    app.tool.create_op = match app.tool.create_op {
        CreateOp::Union => CreateOp::Subtract,
        CreateOp::Subtract => CreateOp::Intersect,
        CreateOp::Intersect => CreateOp::Slice,
        CreateOp::Slice => CreateOp::Union,
    };
    app.toast(&format!("Boolean op → {:?}", app.tool.create_op));
}
