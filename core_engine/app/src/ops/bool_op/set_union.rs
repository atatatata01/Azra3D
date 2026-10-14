//! Op: arm create boolean = Union.

use crate::{CreateOp, FourthLayerApp};

pub fn run(app: &mut FourthLayerApp) {
    app.tool.create_op = CreateOp::Union;
    app.toast("Boolean: Union");
}
