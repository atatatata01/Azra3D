//! Op: join selected poly bodies.

use crate::FourthLayerApp;

pub fn run(app: &mut FourthLayerApp) {
    if app.poly_join_has_candidates() {
        app.poly_join();
    } else {
        app.toast("Join: select 2 or more polygonal objects first");
    }
}
