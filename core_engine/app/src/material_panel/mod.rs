//! Material parameter panel — P0 data model + lowering seam + migration
//! (MATERIAL_PANEL_SPEC.md §3/§4-P0, revised by §9).
//!
//! P0 scope ONLY: the `MaterialPanelDoc` runtime model, serde round-trip, the
//! FL4MPNLS persistence records (see `project.rs`), migration from existing
//! `PbrMaterial` + `NodeGraph` state, and the cost readout. **No UI, no codegen.**
//!
//! Authority model (spec C1, §9): the doc is authored intent; `ShaderType` on
//! `PbrMaterial` IS the shader model (no new shader enum); the node graph is the
//! doc's expansion (P2). At P0 the doc is a passive mirror — nothing reads it to
//! render, so existing materials load and render byte-for-byte identically.

pub(crate) mod edit;
pub(crate) mod layer_ui;
pub(crate) mod lower;
pub(crate) mod model;
pub(crate) mod preset;
pub(crate) mod view;

pub(crate) use edit::MpConfirm;
pub(crate) use model::MaterialPanelDoc;

/// Collect the FL4MPNLS records from the runtime doc store (save-side pure fn —
/// mirrors `project_save::collect_material_channels`). Docs serialize as JSON
/// (like FL4NODES `graph_json`) so field evolution stays `#[serde(default)]`-friendly.
/// Sorted by material id for deterministic bytes. Unserializable docs are skipped
/// (fail-soft — never fails a save).
pub(crate) fn collect_panel_docs(
    docs: &std::collections::HashMap<u32, MaterialPanelDoc>,
) -> Vec<crate::project::MaterialPanelRecord> {
    let mut recs: Vec<crate::project::MaterialPanelRecord> = docs
        .iter()
        .filter_map(|(&mid, doc)| {
            serde_json::to_string(doc)
                .ok()
                .map(|doc_json| crate::project::MaterialPanelRecord {
                    material_id: mid,
                    doc_json,
                })
        })
        .collect();
    recs.sort_by_key(|r| r.material_id);
    recs
}

/// Reattach FL4MPNLS records into the runtime doc store (load-side pure fn —
/// mirrors `project_load::reattach_node_graphs`). Corrupt JSON is skipped with a
/// log line, never a load failure. An ABSENT section (every pre-FL4MPNLS file) is
/// simply an empty store: docs are derived on demand via
/// [`derive_doc_from_material`] when the panel first needs them (migration path).
pub(crate) fn reattach_panel_docs(
    store: &mut std::collections::HashMap<u32, MaterialPanelDoc>,
    recs: &[crate::project::MaterialPanelRecord],
) {
    store.clear();
    for rec in recs {
        match serde_json::from_str::<MaterialPanelDoc>(&rec.doc_json) {
            Ok(doc) => {
                store.insert(rec.material_id, doc);
            }
            Err(e) => eprintln!(
                "[Load] FL4MPNLS: bad panel doc JSON for material {} — skipped ({e})",
                rec.material_id
            ),
        }
    }
}
