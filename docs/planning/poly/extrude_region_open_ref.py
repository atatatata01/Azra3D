# extrude_region_open_ref.py — Blender oracle for REGION extrude on OPEN meshes (verification #2).
# Two scenarios, extrude by +Z 0.5:
#   A: single open quad (z=0) -> lifts as a cap, 4 walls, open bottom. All 4 verts are region-BOUNDARY.
#   B: 3x3-vertex grid = 4 quads, extrude ALL 4 -> the CENTER vertex is truly interior (MOVED not
#      duplicated); the 8 rim verts are boundary (duplicated).
#   blender.exe --background --python extrude_region_open_ref.py -- out.json
import bpy, bmesh, json, sys

out = sys.argv[sys.argv.index("--") + 1:][0]

def run(verts, faces, sel_pred, label):
    bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
    mesh = bpy.data.meshes.new(label); mesh.from_pydata(verts, [], faces); mesh.update()
    obj = bpy.data.objects.new(label, mesh); bpy.context.collection.objects.link(obj)
    bpy.context.view_layer.objects.active = obj
    bpy.ops.object.mode_set(mode='EDIT')
    bpy.ops.mesh.select_mode(type='FACE')
    bpy.ops.mesh.select_all(action='DESELECT')
    bm = bmesh.from_edit_mesh(mesh); bm.faces.ensure_lookup_table()
    for f in bm.faces:
        if sel_pred(f.calc_center_median()): f.select = True
    bmesh.update_edit_mesh(mesh)
    bpy.ops.mesh.extrude_region_move(TRANSFORM_OT_translate={"value": (0.0, 0.0, 0.5)})
    bpy.ops.object.mode_set(mode='OBJECT')
    V = [list(v.co) for v in mesh.vertices]; F = [list(p.vertices) for p in mesh.polygons]
    bnd = sum(1 for e in mesh.edges if len([p for p in mesh.polygons if e.vertices[0] in p.vertices and e.vertices[1] in p.vertices]) < 2)
    print("%s: nv=%d nf=%d lifted_z0.5=%d" % (label, len(V), len(F), sum(1 for v in V if abs(v[2]-0.5) < 1e-4)))
    return {"nv": len(V), "nf": len(F), "verts": V}

# A: single quad
A = run([(-1,-1,0),(1,-1,0),(1,1,0),(-1,1,0)], [(0,1,2,3)], lambda c: True, "A_open_quad")
# B: 3x3 grid (9 verts, 4 quads), extrude all
gv = [(x, y, 0) for y in (-1.0, 0.0, 1.0) for x in (-1.0, 0.0, 1.0)]  # idx = y*3 + x
gf = [(0,1,4,3), (1,2,5,4), (3,4,7,6), (4,5,8,7)]
B = run(gv, gf, lambda c: True, "B_grid_interior")

json.dump({"open_quad": A, "grid": B}, open(out, "w"), indent=1)
