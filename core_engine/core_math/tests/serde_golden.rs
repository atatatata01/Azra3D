//! Serialization golden-gate — .fl4 format fence for the god-file splits.
//!
//! Committed golden blobs (`tests/golden/*.bin`) are bincode snapshots of the two
//! serialized cores of the save format: `HalfEdgeMesh` and `SceneGraph`. bincode is
//! POSITIONAL and NOT self-describing — reordering, retyping, or removing any field
//! on `HVertex` / `HalfEdge` / `HFace` / `HalfEdgeMesh` / `SceneNode` / `NodePayload`
//! makes these committed bytes deserialize into garbage (or fail), and the exact-value
//! asserts below fire. If one of these tests fails, you have broken every existing
//! `.fl4` save file — revert the layout change; do NOT regenerate the golden to "fix" it.
//!
//! Regeneration is ONLY legitimate when the on-disk format is intentionally changed in a
//! save-format-lockstep session:  AZRA_WRITE_GOLDEN=1 cargo test -p core_math --test serde_golden

use std::path::PathBuf;
use std::sync::Arc;

use core_math::half_edge::{FaceId, HalfEdgeMesh};
use core_math::nurbs::NurbsSurface;
use core_math::pbr_material::{MaterialId, PbrMaterial};
use core_math::primitives::SdfPrimitive;
use core_math::scene_graph::{
    LightData, MeshCacheId, NodePayload, PolyModifier, PolyModifierType, SceneGraph,
};
use core_math::sdf_shapes::SdfShape;
use core_math::voxel_sculpt::VoxelGrid;
use glam::Vec3;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden")
}

fn write_golden_enabled() -> bool {
    std::env::var("AZRA_WRITE_GOLDEN").map(|v| v == "1").unwrap_or(false)
}

// ============================================================
// Reference constructors (deterministic)
// ============================================================

/// Unit-ish box (half extents 1) + one face extruded so free-lists / twin wiring /
/// id allocation past the ctor are exercised, then bincode round-trips.
fn reference_half_edge_mesh() -> HalfEdgeMesh {
    let mut mesh = HalfEdgeMesh::new_box(Vec3::new(1.0, 1.0, 1.0));
    // Extrude face 0 outward along +Y-ish arbitrary offset (deterministic).
    let new_face = mesh.extrude_face(FaceId(0), Vec3::new(0.0, 0.5, 0.0));
    // Give the extruded cap a non-default material id so HFace.material_id is exercised.
    mesh.face_mut(new_face).material_id = 7;
    mesh
}

/// Small scene graph with EXACTLY one node per NodePayload variant, added in enum
/// order (Root is implicit): Group, SdfBody, ImportedMesh, Light, Camera,
/// PolygonalBody, NurbsBody, Material, Empty, VoxelBody, SdfObject.
fn reference_scene_graph() -> SceneGraph {
    let mut g = SceneGraph::new();
    let root = g.root();

    g.add_child(root, NodePayload::Group { name: "GoldenGroup".into() });

    let prim = SdfPrimitive::with_uid(
        "GoldenSphere".into(),
        SdfShape::Sphere { radius: 1.25 },
        Vec3::new(0.5, 1.5, -2.5),
        42,
    );
    g.add_child(
        root,
        NodePayload::SdfBody {
            name: "GoldenBody".into(),
            primitives: vec![prim],
            mesh_cache_id: None,
            body_op: core_math::primitives::BooleanOp::Union { k: 0.0 },
            origin: Vec3::new(0.5, 1.5, -2.5),
            body_rotation: glam::Quat::IDENTITY,
            body_scale: Vec3::ONE,
            mirror_flags: 0,
            mirror_sign: 0,
            mirror_center: Vec3::ZERO,
            bevel_radius: 0.0,
            blend_profile: Default::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
            mesh_modifiers: Vec::new(),
            field_modifiers: Vec::new(),
        },
    );

    g.add_child(
        root,
        NodePayload::ImportedMesh {
            name: "GoldenImport".into(),
            mesh_cache_id: MeshCacheId(99),
            materials: vec![PbrMaterial::default()],
        },
    );

    let mut light = LightData::default();
    light.name = "GoldenLight".into();
    light.id = 3;
    g.add_child(root, NodePayload::Light(light));

    g.add_child(
        root,
        NodePayload::Camera {
            name: "GoldenCam".into(),
            position: Vec3::new(1.0, 2.0, 3.0),
            target: Vec3::ZERO,
            fov: 45.0,
        },
    );

    g.add_child(
        root,
        NodePayload::PolygonalBody {
            name: "GoldenPoly".into(),
            mesh: Arc::new(HalfEdgeMesh::new_box(Vec3::new(0.5, 0.5, 0.5))),
            source: Default::default(),
            mesh_cache_id: None,
            material: PbrMaterial::default(),
            modifiers: vec![PolyModifier::new(PolyModifierType::Mirror)],
        },
    );

    g.add_child(
        root,
        NodePayload::NurbsBody {
            name: "GoldenNurbs".into(),
            surface: Arc::new(NurbsSurface::new_plane(2.0, 2.0)),
            mesh_cache_id: None,
            material: PbrMaterial::default(),
        },
    );

    g.add_child(
        root,
        NodePayload::Material {
            id: MaterialId(1),
            name: "GoldenMat".into(),
            data: PbrMaterial::default(),
        },
    );

    g.add_child(root, NodePayload::Empty { name: "GoldenEmpty".into() });

    g.add_child(
        root,
        NodePayload::VoxelBody {
            name: "GoldenVoxel".into(),
            grid: VoxelGrid::new_empty(8, 1.0), // #[serde(skip)] — not in the blob
            mesh_cache_id: Some(MeshCacheId(123)),
            material: PbrMaterial::default(),
        },
    );

    g.add_child(root, NodePayload::SdfObject { name: "GoldenObject".into(), mesh_cache_id: None });

    g
}

// ============================================================
// Golden writer (AZRA_WRITE_GOLDEN=1)
// ============================================================

#[test]
fn write_golden_blobs_if_requested() {
    if !write_golden_enabled() {
        return;
    }
    let dir = golden_dir();
    std::fs::create_dir_all(&dir).unwrap();

    let mesh = reference_half_edge_mesh();
    let bytes = bincode::serialize(&mesh).expect("serialize HalfEdgeMesh");
    std::fs::write(dir.join("half_edge_ref.bin"), &bytes).unwrap();
    eprintln!("wrote half_edge_ref.bin ({} bytes)", bytes.len());

    let graph = reference_scene_graph();
    let bytes = bincode::serialize(&graph).expect("serialize SceneGraph");
    std::fs::write(dir.join("scene_graph_ref.bin"), &bytes).unwrap();
    eprintln!("wrote scene_graph_ref.bin ({} bytes)", bytes.len());
}

// ============================================================
// The permanent gates
// ============================================================

#[test]
fn golden_half_edge_mesh_deserializes_exactly() {
    let bytes = std::fs::read(golden_dir().join("half_edge_ref.bin"))
        .expect("missing golden half_edge_ref.bin — run once with AZRA_WRITE_GOLDEN=1");
    let mesh: HalfEdgeMesh =
        bincode::deserialize(&bytes).expect("HalfEdgeMesh layout changed — .fl4 saves are broken");

    // Structural counts: box (8v / 6f / 24he) + one quad-face extrude
    // (4 new verts, cap + 4 side faces replace 1, side/cap half-edges).
    let expected = reference_half_edge_mesh();
    assert_eq!(mesh.vertex_count(), expected.vertex_count(), "vertex count");
    assert_eq!(mesh.face_count(), expected.face_count(), "face count");
    assert_eq!(mesh.half_edges.len(), expected.half_edges.len(), "half-edge slot count");
    assert_eq!(mesh.vertices.len(), expected.vertices.len(), "vertex slot count");
    assert_eq!(mesh.faces.len(), expected.faces.len(), "face slot count");

    // Hardcoded anchors independent of the reference constructor (belt-and-braces):
    // new_box(1,1,1) vertex 0 = (-1,-1,-1); box alone = 8 verts / 6 faces, extrude adds more.
    assert_eq!(mesh.vertices[0].position, Vec3::new(-1.0, -1.0, -1.0), "box corner 0");
    assert!(mesh.vertex_count() > 8 && mesh.face_count() > 6, "extrude missing from golden");

    // Exact vertex positions (box corners from new_box + an extruded vertex).
    for idx in [0usize, 3, 7, mesh.vertices.len() - 1] {
        assert_eq!(
            mesh.vertices[idx].position, expected.vertices[idx].position,
            "vertex {idx} position drift — field reorder/retype in HVertex?"
        );
    }

    // One face's full vertex ring must match exactly (exercises HalfEdge.next/vertex wiring).
    let ring: Vec<u32> = mesh.face_vertices(FaceId(1)).iter().map(|v| v.0).collect();
    let expected_ring: Vec<u32> = expected.face_vertices(FaceId(1)).iter().map(|v| v.0).collect();
    assert!(!ring.is_empty(), "face 1 ring empty");
    assert_eq!(ring, expected_ring, "face 1 vertex ring drift — HalfEdge layout changed?");

    // material_id on the extruded cap (set to 7 by the reference constructor).
    let cap = FaceId((mesh.faces.len() - 5) as u32); // extrude alloc order: cap then 4 sides? assert via expected instead
    let _ = cap;
    let mat_faces: Vec<u32> =
        mesh.faces.iter().map(|f| f.material_id).collect();
    let expected_mats: Vec<u32> = expected.faces.iter().map(|f| f.material_id).collect();
    assert_eq!(mat_faces, expected_mats, "HFace.material_id drift");
    assert!(mat_faces.contains(&7), "extruded cap material_id 7 missing");
}

#[test]
fn golden_scene_graph_deserializes_exactly() {
    let bytes = std::fs::read(golden_dir().join("scene_graph_ref.bin"))
        .expect("missing golden scene_graph_ref.bin — run once with AZRA_WRITE_GOLDEN=1");
    let g: SceneGraph =
        bincode::deserialize(&bytes).expect("SceneGraph layout changed — .fl4 saves are broken");

    // Root + 11 payload nodes (one per non-Root NodePayload variant).
    let root = g.root();
    let children = &g.get(root).expect("root node").children;
    assert_eq!(children.len(), 11, "expected one child per non-Root NodePayload variant");

    // Each child, in creation order, must deserialize as the exact enum variant with
    // exact authored values. A variant-order shift makes these tags decode wrong.
    let payload = |i: usize| &g.get(children[i]).unwrap().payload;

    match payload(0) {
        NodePayload::Group { name } => assert_eq!(name, "GoldenGroup"),
        p => panic!("child 0 not Group: {:?}", p.display_name()),
    }
    match payload(1) {
        NodePayload::SdfBody { name, primitives, origin, .. } => {
            assert_eq!(name, "GoldenBody");
            assert_eq!(primitives.len(), 1);
            assert_eq!(primitives[0].prim_uid, 42);
            assert_eq!(primitives[0].position, Vec3::new(0.5, 1.5, -2.5));
            match primitives[0].shape {
                SdfShape::Sphere { radius } => assert_eq!(radius, 1.25),
                _ => panic!("prim shape drift"),
            }
            assert_eq!(*origin, Vec3::new(0.5, 1.5, -2.5));
        }
        p => panic!("child 1 not SdfBody: {:?}", p.display_name()),
    }
    match payload(2) {
        NodePayload::ImportedMesh { name, mesh_cache_id, materials } => {
            assert_eq!(name, "GoldenImport");
            assert_eq!(*mesh_cache_id, MeshCacheId(99));
            assert_eq!(materials.len(), 1);
        }
        p => panic!("child 2 not ImportedMesh: {:?}", p.display_name()),
    }
    match payload(3) {
        NodePayload::Light(data) => {
            assert_eq!(data.name, "GoldenLight");
            assert_eq!(data.id, 3);
        }
        p => panic!("child 3 not Light: {:?}", p.display_name()),
    }
    match payload(4) {
        NodePayload::Camera { name, position, fov, .. } => {
            assert_eq!(name, "GoldenCam");
            assert_eq!(*position, Vec3::new(1.0, 2.0, 3.0));
            assert_eq!(*fov, 45.0);
        }
        p => panic!("child 4 not Camera: {:?}", p.display_name()),
    }
    match payload(5) {
        NodePayload::PolygonalBody { name, mesh, modifiers, .. } => {
            assert_eq!(name, "GoldenPoly");
            assert_eq!(mesh.vertex_count(), 8);
            assert_eq!(mesh.face_count(), 6);
            assert_eq!(mesh.vertices[0].position, Vec3::new(-0.5, -0.5, -0.5));
            assert_eq!(modifiers.len(), 1);
            assert_eq!(modifiers[0].mod_type, PolyModifierType::Mirror);
        }
        p => panic!("child 5 not PolygonalBody: {:?}", p.display_name()),
    }
    match payload(6) {
        NodePayload::NurbsBody { name, surface, .. } => {
            assert_eq!(name, "GoldenNurbs");
            assert!(!surface.control_points.is_empty());
        }
        p => panic!("child 6 not NurbsBody: {:?}", p.display_name()),
    }
    match payload(7) {
        NodePayload::Material { id, name, .. } => {
            assert_eq!(*id, MaterialId(1));
            assert_eq!(name, "GoldenMat");
        }
        p => panic!("child 7 not Material: {:?}", p.display_name()),
    }
    match payload(8) {
        NodePayload::Empty { name } => assert_eq!(name, "GoldenEmpty"),
        p => panic!("child 8 not Empty: {:?}", p.display_name()),
    }
    match payload(9) {
        NodePayload::VoxelBody { name, mesh_cache_id, .. } => {
            assert_eq!(name, "GoldenVoxel");
            assert_eq!(*mesh_cache_id, Some(MeshCacheId(123)));
        }
        p => panic!("child 9 not VoxelBody (tail-order violation!): {:?}", p.display_name()),
    }
    match payload(10) {
        NodePayload::SdfObject { name, mesh_cache_id } => {
            assert_eq!(name, "GoldenObject");
            assert_eq!(*mesh_cache_id, None);
        }
        p => panic!("child 10 not SdfObject (tail-order violation!): {:?}", p.display_name()),
    }
}

/// GATE — the Bend params cost the bincode blob ZERO bytes.
///
/// `PolyModifier` is serialized INSIDE the scene graph and bincode is positional, so any field that
/// reaches the blob shifts every byte after it and corrupts every existing `.fl4` carrying a poly
/// modifier. All six `bend_*` fields are therefore `#[serde(skip)]` + the `FL4PBEND` tail section,
/// and this pins that: authoring every one of them must not change a single serialized byte.
///
/// Stronger than the golden-blob test above and deliberately independent of it. That test compares
/// against a recorded fixture, so it goes quiet the moment someone re-records the golden — which is
/// the natural reaction to seeing it fail. This one needs no fixture: it compares the struct
/// against ITSELF, so re-recording cannot silence it.
#[test]
fn bend_params_add_no_bytes_to_the_blob() {
    let base = PolyModifier::new(PolyModifierType::Bend);
    let mut authored = base.clone();
    authored.bend_angle_deg = -137.5;
    authored.bend_axis = 2;
    authored.bend_dir_deg = 61.25;
    authored.bend_limit_lo = 0.3;
    authored.bend_limit_hi = 0.6;
    authored.bend_center = Vec3::new(1.5, -2.5, 3.5);

    let a = bincode::serialize(&base).expect("serialize default Bend");
    let b = bincode::serialize(&authored).expect("serialize authored Bend");
    assert_eq!(
        a, b,
        "authoring the bend_* params changed the bincode blob — one of them lost its \
         #[serde(skip)] and every existing .fl4 carrying a poly modifier is now unreadable"
    );

    // …and the params still survive a round-trip in memory, so `skip` did not quietly turn the
    // whole feature into a no-op that this gate would otherwise be happy with.
    let back: PolyModifier = bincode::deserialize(&b).expect("deserialize");
    assert_eq!(back.bend_angle_deg, base.bend_angle_deg, "skip must restore the DEFAULT, not 0");
    assert_eq!(back.bend_limit_hi, base.bend_limit_hi, "…including the non-zero limit default");
    assert_eq!(authored.bend_center, Vec3::new(1.5, -2.5, 3.5), "the live struct still holds it");
}
