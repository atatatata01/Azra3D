# Blender oracle for AZRA's EDGE-MODE "Circle" op (Alt+C in Edge mode).
#
#   blender.exe --background --python circle_edge_ref.py -- circle_edge_ref.json
#
# Reference op: LoopTools "Circle" (bundled extension) run on a selected EDGE LOOP (edge select mode),
# at its UI DEFAULTS: fit='best' custom_radius=False radius=1.0 flatten=True regular=True.
#
# INPUT PARITY: same generation formula as HalfEdgeMesh::new_plane(2,2,4,4) — a 4x4 quad grid in the XZ
# plane at y=0 (25 verts, 16 quads). The "asym" case displaces vert 10 -> (-0.6,0,0) so the
# closest-to-centroid anchor is UNIQUE (per-vertex identity is then well defined).
#
# EDGE-LOOP SELECTION: we select the OUTER BOUNDARY edge loop (bmesh e.is_boundary). Those are exactly
# the 16 boundary vertices of the region. Proving edge-mode Circle reshapes THIS loop identically to
# face-mode Circle over the whole region is the "one producer" guarantee, by vertex identity.

import bpy, bmesh, addon_utils, json, sys, math

addon_utils.enable("bl_ext.blender_org.looptools", default_set=False, persistent=False)

SUB = 4
SIZE = 2.0
EXTENT = SIZE * 0.5


def grid_verts_faces():
    verts = []
    for iz in range(SUB + 1):
        for ix in range(SUB + 1):
            x = -EXTENT + (ix / SUB) * SIZE
            z = -EXTENT + (iz / SUB) * SIZE
            verts.append((x, 0.0, z))
    faces = []
    for iz in range(SUB):
        for ix in range(SUB):
            a = iz * (SUB + 1) + ix
            faces.append((a, a + SUB + 1, a + SUB + 2, a + 1))
    return verts, faces


def clear():
    bpy.ops.object.select_all(action='SELECT')
    bpy.ops.object.delete()


def build(verts, faces, name):
    me = bpy.data.meshes.new(name)
    me.from_pydata([list(v) for v in verts], [], [list(f) for f in faces])
    me.update()
    ob = bpy.data.objects.new(name, me)
    bpy.context.collection.objects.link(ob)
    bpy.context.view_layer.objects.active = ob
    ob.select_set(True)
    return ob


def mlen(a, b):
    return math.sqrt(sum((a[i] - b[i]) ** 2 for i in range(3)))


def run_edge_circle(ob, **kw):
    # EDIT mode, EDGE select mode, select only the outer boundary edge loop, run LoopTools Circle.
    bpy.ops.object.mode_set(mode='EDIT')
    bpy.ops.mesh.select_mode(type='EDGE')
    bpy.ops.mesh.select_all(action='DESELECT')
    bm = bmesh.from_edit_mesh(ob.data)
    bm.edges.ensure_lookup_table()
    sel_edges = []
    for e in bm.edges:
        if e.is_boundary:
            e.select = True
            sel_edges.append((e.verts[0].index, e.verts[1].index))
    bmesh.update_edit_mesh(ob.data)
    res = bpy.ops.mesh.looptools_circle(**kw)
    bpy.ops.object.mode_set(mode='OBJECT')
    return res, sel_edges


def case(name, displace=None, **kw):
    clear()
    verts, faces = grid_verts_faces()
    if displace:
        for idx, pos in displace.items():
            verts[idx] = pos
    ob = build(verts, faces, name)
    before = [list(v.co) for v in ob.data.vertices]
    _, sel_edges = run_edge_circle(ob, **kw)
    after = [[round(c, 6) for c in v.co] for v in ob.data.vertices]
    moved = [i for i in range(len(after)) if mlen(after[i], before[i]) > 1e-6]
    return {
        "settings": {k: v for k, v in kw.items()},
        "selected_edges_by_vidx": sorted([tuple(sorted(e)) for e in sel_edges]),
        "input": [[round(c, 6) for c in v] for v in before],
        "verts": after,
        "nv": len(after),
        "nf": len(ob.data.polygons),
        "moved_indices": moved,
    }


out = {}
# Edge-loop Circle on the asym grid's outer boundary loop (unique anchor => per-vertex identity).
out["asym_edge"] = case("asym_edge", displace={10: (-0.6, 0.0, 0.0)})
# Same but regular=False (each vertex keeps its own angle).
out["asym_edge_nonregular"] = case("asym_edge_nonregular", displace={10: (-0.6, 0.0, 0.0)}, regular=False)

path = sys.argv[-1] if sys.argv[-1].endswith(".json") else "circle_edge_ref.json"
with open(path, "w") as f:
    json.dump(out, f, indent=1)
print("WROTE", path)
for k, v in out.items():
    print(k, "nv", v["nv"], "nf", v["nf"], "moved", len(v["moved_indices"]),
          "sel_edges", len(v["selected_edges_by_vidx"]))
