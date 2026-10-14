# inset_region_ref.py — Blender oracle for INSET REGION vs INSET INDIVIDUAL (POLY_POLISH_PLAN §4 2e).
#
# Blender's `I` DEFAULT is REGION (use_individual=False): a multi-face selection is treated as ONE
# region — only its OUTER boundary is inset, the shared INTERIOR edge is not. Pressing I,I (or ticking
# "Individual") insets each face separately, each getting its own full ring.
# AZRA currently always does the Individual behaviour, so multi-face inset diverges from Blender's default.
#
# INPUT: box half-extents (2,1,1) with the +Y top SPLIT along z=0 into two 4x1 quads (both NON-square,
# so even-offset stays discriminating). Verts 8,9 sit mid-edge on the -X/+X side faces (pentagons).
#
# Settings pinned: bpy.ops.mesh.inset(thickness=0.3, depth=0.0), use_even_offset default (True).
#   blender.exe --background --python inset_region_ref.py -- out.json
import bpy, bmesh, json, sys

out = sys.argv[sys.argv.index("--") + 1:][0]

VERTS = [(-2,-1,-1),(2,-1,-1),(2,-1,1),(-2,-1,1),(-2,1,-1),(2,1,-1),(2,1,1),(-2,1,1),(-2,1,0),(2,1,0)]
FACES = [(0,1,2,3),        # bottom
         (4,5,9,8),        # top A (z -1..0)  4x1
         (8,9,6,7),        # top B (z  0..1)  4x1
         (0,1,5,4),        # -Z
         (3,7,6,2),        # +Z
         (0,3,7,8,4),      # -X (pentagon: 8 is mid-edge)
         (1,5,9,6,2)]      # +X (pentagon: 9 is mid-edge)

def build():
    bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
    mesh = bpy.data.meshes.new("m"); mesh.from_pydata(VERTS, [], FACES); mesh.update()
    obj = bpy.data.objects.new("o", mesh); bpy.context.collection.objects.link(obj)
    bpy.context.view_layer.objects.active = obj
    bpy.ops.object.mode_set(mode='EDIT')
    bpy.ops.mesh.select_all(action='SELECT')
    bpy.ops.mesh.normals_make_consistent(inside=False)
    bpy.ops.mesh.select_all(action='DESELECT')
    bpy.ops.mesh.select_mode(type='FACE')
    bm = bmesh.from_edit_mesh(mesh); bm.faces.ensure_lookup_table()
    for f in bm.faces:
        if abs(f.calc_center_median().y - 1.0) < 1e-4:   # BOTH +Y top faces
            f.select = True
    bmesh.update_edit_mesh(mesh)
    return mesh

def dump(mesh):
    bpy.ops.object.mode_set(mode='OBJECT')
    V = [[round(c, 6) for c in v.co] for v in mesh.vertices]
    F = [list(p.vertices) for p in mesh.polygons]
    top = sorted([v for v in V if abs(v[1] - 1.0) < 1e-4])
    return {"nv": len(V), "nf": len(F), "verts": V, "faces": F, "top_ring": top}

res = {}

# --- REGION (Blender's I default) ---
m = build()
bpy.ops.mesh.inset(thickness=0.3, depth=0.0, use_individual=False)
res["region"] = dump(m)

# --- INDIVIDUAL (I,I) ---
m = build()
bpy.ops.mesh.inset(thickness=0.3, depth=0.0, use_individual=True)
res["individual"] = dump(m)

json.dump(res, open(out, "w"), indent=1)
print("REGION    : nv=%d nf=%d" % (res["region"]["nv"], res["region"]["nf"]))
print("INDIVIDUAL: nv=%d nf=%d" % (res["individual"]["nv"], res["individual"]["nf"]))
print("REGION top-plane verts:")
for p in res["region"]["top_ring"]: print("   ", p)
print("INDIVIDUAL top-plane verts:")
for p in res["individual"]["top_ring"]: print("   ", p)
