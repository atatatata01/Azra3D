//! `PolyEdit::targets()` — the mapping the app's refusal handling stands on.
//!
//! When a poly op REFUSES, `poly_report_batch` (app/src/poly/outcome.rs) keeps exactly the refused
//! op's target elements selected so the user can see and retry them. That retention is only correct
//! if `targets()` echoes back the SAME ids, in the SAME currency, the caller handed in. These tests
//! pin that contract; they are the app-layer behavior's only automated coverage (the app crate has
//! no test harness for `AzraApp` — the toast + selection halves are manual-test only).

use super::{ElemDomain, PolyEdit};

/// THE retention contract: `targets()` returns exactly the ids the variant was constructed with.
/// If this ever drifts, refused elements silently drop out of the selection — the original bug.
#[test]
fn targets_echo_the_ids_handed_in() {
    let cases: Vec<(PolyEdit, ElemDomain, Vec<u32>)> = vec![
        (PolyEdit::DissolveEdge { edge: 7 }, ElemDomain::HalfEdge, vec![7]),
        (PolyEdit::DeleteEdge { edge: 3 }, ElemDomain::HalfEdge, vec![3]),
        (PolyEdit::SplitEdge { edge: 11, t: 0.5 }, ElemDomain::HalfEdge, vec![11]),
        (PolyEdit::LoopCut { start_edge: 2 }, ElemDomain::HalfEdge, vec![2]),
        (PolyEdit::DeleteVertex { vertex: 5 }, ElemDomain::Vertex, vec![5]),
        (PolyEdit::DissolveVertex { vertex: 6 }, ElemDomain::Vertex, vec![6]),
        (PolyEdit::MergeVertices { a: 1, b: 4 }, ElemDomain::Vertex, vec![1, 4]),
        (PolyEdit::FlipFace { face: 9 }, ElemDomain::Face, vec![9]),
        (PolyEdit::DeleteFace { face: 0 }, ElemDomain::Face, vec![0]),
        (PolyEdit::DissolveFace { face: 8 }, ElemDomain::Face, vec![8]),
        (PolyEdit::ExtrudeFace { face: 12, offset: [0.0, 1.0, 0.0] }, ElemDomain::Face, vec![12]),
        (PolyEdit::InsetFace { face: 13, amount: 0.1 }, ElemDomain::Face, vec![13]),
    ];
    for (op, want_domain, want_ids) in cases {
        let (domain, ids) = op.targets().unwrap_or_else(|| panic!("{op:?} must target elements"));
        assert_eq!(domain, want_domain, "wrong domain for {op:?}");
        assert_eq!(ids, want_ids, "wrong ids for {op:?}");
    }
}

/// A REGION op covers N elements through ONE op. The app counts elements (not ops) from these ids —
/// counting ops would report "1 face dissolved" after dissolving five, and would retain only part of
/// a refused region's selection.
#[test]
fn region_ops_target_every_element_they_cover() {
    let faces = vec![4u32, 5, 6, 7, 8];
    let region_ops = vec![
        PolyEdit::DissolveFaces { faces: faces.clone() },
        PolyEdit::SubdivideFaces { faces: faces.clone() },
        PolyEdit::ExtrudeRegion { faces: faces.clone(), offset: [0.0, 1.0, 0.0] },
        PolyEdit::InsetRegion { faces: faces.clone(), thickness: 0.1, depth: 0.0 },
        PolyEdit::DeleteFacesOnly { faces: faces.clone() },
        PolyEdit::DeleteFaces { faces: faces.clone() },
    ];
    for op in region_ops {
        let (domain, ids) = op.targets().unwrap_or_else(|| panic!("{op:?} must target faces"));
        assert_eq!(domain, ElemDomain::Face, "{op:?}");
        assert_eq!(ids, faces, "{op:?} must return ALL covered faces, not just the first");
    }

    let verts = vec![2u32, 3, 9];
    let op = PolyEdit::MergeVerticesAt { verts: verts.clone(), target: [0.0; 3] };
    let (domain, ids) = op.targets().expect("MergeVerticesAt must target vertices");
    assert_eq!(domain, ElemDomain::Vertex);
    assert_eq!(ids, verts, "a refused merge must retain its WHOLE cluster, not just the survivor");
}

/// Whole-mesh ops own no selection domain — the reporter must not clear a set on their behalf.
#[test]
fn whole_mesh_ops_target_nothing() {
    assert!(PolyEdit::SubdivideCatmullClark.targets().is_none());
    assert!(PolyEdit::Solidify { thickness: 0.1, offset: -1.0 }.targets().is_none());
}
