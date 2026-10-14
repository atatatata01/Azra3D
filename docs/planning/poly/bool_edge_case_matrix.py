# bool_edge_case_matrix.py — Blender EXACT = source of truth for poly boolean edge cases.
#
# Same discipline as the bevel oracle matrix: every case dumps topology invariants
# (nv, ne, nf, face-size hist, non-manifold count, euler, bbox) so core_math::mesh_boolean
# can assert start→end parity.
#
#   "C:\Program Files\Blender Foundation\Blender 5.0\blender.exe" --background --python bool_edge_case_matrix.py -- bool_edge_case_matrix.json
#
import bpy, bmesh, json, sys, math
from collections import Counter

out = sys.argv[sys.argv.index("--") + 1:][0]


def clean():
    # Background + factory-startup: avoid ops that need VIEW_3D context.
    for obj in list(bpy.data.objects):
        bpy.data.objects.remove(obj, do_unlink=True)
    for mesh in list(bpy.data.meshes):
        bpy.data.meshes.remove(mesh)


def apply_bool(A, B, op):
    m = A.modifiers.new("b", "BOOLEAN")
    m.operation = op
    m.object = B
    m.solver = "EXACT"
    bpy.context.view_layer.objects.active = A
    bpy.ops.object.modifier_apply(modifier="b")
    bpy.data.objects.remove(B, do_unlink=True)


def census(obj, label):
    bm = bmesh.new()
    bm.from_mesh(obj.data)
    bm.edges.ensure_lookup_table()
    bm.faces.ensure_lookup_table()
    bm.verts.ensure_lookup_table()
    # Weld/merge-by-distance is NOT applied — Exact boolean output as-is (truth).
    nv = len(bm.verts)
    ne = len(bm.edges)
    nf = len(bm.faces)
    sizes = {}
    for f in bm.faces:
        n = len(f.verts)
        sizes[str(n)] = sizes.get(str(n), 0) + 1
    # Non-manifold: edge face count != 2
    nonman = 0
    boundary = 0
    for e in bm.edges:
        c = len(e.link_faces)
        if c != 2:
            nonman += 1
        if c == 1:
            boundary += 1
    # Bbox
    if nv == 0:
        bmin = bmax = [0, 0, 0]
    else:
        coords = [v.co for v in bm.verts]
        bmin = [min(c[i] for c in coords) for i in range(3)]
        bmax = [max(c[i] for c in coords) for i in range(3)]
    euler = nv - ne + nf
    tris = sizes.get("3", 0)
    bm.free()
    return {
        "label": label,
        "nv": nv,
        "ne": ne,
        "nf": nf,
        "euler": euler,
        "non_manifold_edges": nonman,
        "boundary_edges": boundary,
        "tris": tris,
        "face_sizes": sizes,
        "bbox_min": bmin,
        "bbox_max": bmax,
    }


def case_cube_minus_corner():
    """A=cube[-1,1], B=cube center(1,1,1) half=0.7 → corner bite. Classic clean n-gons."""
    clean()
    bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0, 0, 0))
    A = bpy.context.active_object
    bpy.ops.mesh.primitive_cube_add(size=1.4, location=(1, 1, 1))
    B = bpy.context.active_object
    apply_bool(A, B, "DIFFERENCE")
    return census(A, "cube_minus_corner")


def case_cube_minus_rotcube():
    """Cube minus 45° rotated cube offset on X — off-axis coplanar dissolve."""
    clean()
    bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0, 0, 0))
    A = bpy.context.active_object
    bpy.ops.mesh.primitive_cube_add(
        size=2.0, location=(1.2, 0, 0), rotation=(0, 0, math.radians(45))
    )
    B = bpy.context.active_object
    apply_bool(A, B, "DIFFERENCE")
    return census(A, "cube_minus_rotcube")


def case_cube_minus_cylinder():
    """Cube minus 16-gon cylinder through Z — curved wall + flat dissolve."""
    clean()
    bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0, 0, 0))
    A = bpy.context.active_object
    bpy.ops.mesh.primitive_cylinder_add(
        radius=0.5, depth=3.0, vertices=16, location=(0, 0, 0)
    )
    B = bpy.context.active_object
    apply_bool(A, B, "DIFFERENCE")
    return census(A, "cube_minus_cyl16")


def case_coplanar_union():
    """Two boxes sharing face x=1 → union should be one clean solid, no doubled face."""
    clean()
    bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0, 0, 0))  # x∈[-1,1]
    A = bpy.context.active_object
    bpy.ops.mesh.primitive_cube_add(size=2.0, location=(2, 0, 0))  # x∈[1,3]
    B = bpy.context.active_object
    apply_bool(A, B, "UNION")
    return census(A, "coplanar_union")


def case_seq_double_subtract():
    """Sequential cuts over coplanar seam x=0 (the messy 2nd-boolean class)."""
    clean()
    bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0, 0, 0))
    A = bpy.context.active_object
    bpy.ops.mesh.primitive_cube_add(size=2.0, location=(1, 1, 1))
    C1 = bpy.context.active_object
    bpy.ops.mesh.primitive_cube_add(size=2.0, location=(-1, 1, 1))
    C2 = bpy.context.active_object
    apply_bool(A, C1, "DIFFERENCE")
    c1 = census(A, "seq_after_cut1")
    # C2 still exists
    m = A.modifiers.new("b2", "BOOLEAN")
    m.operation = "DIFFERENCE"
    m.object = C2
    m.solver = "EXACT"
    bpy.context.view_layer.objects.active = A
    bpy.ops.object.modifier_apply(modifier="b2")
    bpy.data.objects.remove(C2, do_unlink=True)
    c2 = census(A, "seq_after_cut2")
    return {"cut1": c1, "cut2": c2}


def case_intersect_overlap():
    clean()
    bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0, 0, 0))
    A = bpy.context.active_object
    bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0.5, 0.5, 0.5))
    B = bpy.context.active_object
    apply_bool(A, B, "INTERSECT")
    return census(A, "intersect_overlap")


def case_union_overlap():
    clean()
    bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0, 0, 0))
    A = bpy.context.active_object
    bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0.5, 0.5, 0.5))
    B = bpy.context.active_object
    apply_bool(A, B, "UNION")
    return census(A, "union_overlap")


def case_cube_minus_sphere_approx():
    """Ico sphere cutter — stress curved intersection + residual edges."""
    clean()
    bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0, 0, 0))
    A = bpy.context.active_object
    bpy.ops.mesh.primitive_ico_sphere_add(radius=0.85, subdivisions=2, location=(0.6, 0, 0))
    B = bpy.context.active_object
    apply_bool(A, B, "DIFFERENCE")
    return census(A, "cube_minus_ico")


def case_torus_minus_cube():
    """Revolve-like solid (torus) minus cube — rim residual-edge class (user screenshot)."""
    clean()
    bpy.ops.mesh.primitive_torus_add(
        major_radius=1.0,
        minor_radius=0.35,
        major_segments=16,
        minor_segments=8,
        location=(0, 0, 0),
    )
    A = bpy.context.active_object
    bpy.ops.mesh.primitive_cube_add(size=1.2, location=(1.0, 0, 0))
    B = bpy.context.active_object
    apply_bool(A, B, "DIFFERENCE")
    return census(A, "torus_minus_cube")


def case_cube_minus_half():
    """Axis-aligned half cut — should leave ZERO triangles on the cut face after dissolve."""
    clean()
    bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0, 0, 0))
    A = bpy.context.active_object
    bpy.ops.mesh.primitive_cube_add(size=2.0, location=(1.0, 0, 0))  # removes x>0 half-ish
    B = bpy.context.active_object
    apply_bool(A, B, "DIFFERENCE")
    return census(A, "cube_minus_half")


results = {
    "solver": "EXACT",
    "blender": bpy.app.version_string,
    "cases": {
        "cube_minus_corner": case_cube_minus_corner(),
        "cube_minus_rotcube": case_cube_minus_rotcube(),
        "cube_minus_cyl16": case_cube_minus_cylinder(),
        "coplanar_union": case_coplanar_union(),
        "seq_double_subtract": case_seq_double_subtract(),
        "intersect_overlap": case_intersect_overlap(),
        "union_overlap": case_union_overlap(),
        "cube_minus_ico": case_cube_minus_sphere_approx(),
        "torus_minus_cube": case_torus_minus_cube(),
        "cube_minus_half": case_cube_minus_half(),
    },
}

with open(out, "w") as f:
    json.dump(results, f, indent=2)

print("=== BLENDER EXACT BOOLEAN EDGE-CASE MATRIX ===")
for k, v in results["cases"].items():
    if isinstance(v, dict) and "nv" in v:
        print(
            f"  {k}: nv={v['nv']} ne={v['ne']} nf={v['nf']} tris={v['tris']} "
            f"nonman={v['non_manifold_edges']} euler={v['euler']} sizes={v['face_sizes']}"
        )
    else:
        print(f"  {k}: {v}")
print("Wrote", out)
