//! Op: arm create boolean = Slice.

use crate::{CreateOp, FourthLayerApp};

pub fn run(app: &mut FourthLayerApp) {
    app.tool.create_op = CreateOp::Slice;
    app.toast("Boolean: Slice");
}
