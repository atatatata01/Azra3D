//! Op: arm create boolean = Subtract.

use crate::{CreateOp, FourthLayerApp};

pub fn run(app: &mut FourthLayerApp) {
    app.tool.create_op = CreateOp::Subtract;
    app.toast("Boolean: Subtract");
}
