//! Op: arm create boolean = Intersect.

use crate::{CreateOp, FourthLayerApp};

pub fn run(app: &mut FourthLayerApp) {
    app.tool.create_op = CreateOp::Intersect;
    app.toast("Boolean: Intersect");
}
