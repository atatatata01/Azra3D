# k2d3_corner_ref.py — Blender oracle for the k=2 AT DEGREE-3 rounded corner (the owner's
# live-console FLAT-CLAMP class 2026-07-23: bev=[t,t,f] at an original box corner — the
# canonical "bevel a face's edge loop" workflow). Two scenarios in one run:
#   A) TWO ADJACENT edges meeting at corner (1,1,1) — the minimal k=2 d3 corner.
#   B) the FOUR edges of the +Z face (edge loop) — every top corner is k=2 d3.
#   "...blender.exe" --background --factory-startup --python k2d3_corner_ref.py -- out_prefix
import bpy, bmesh, json, sys

prefix = sys.argv[sys.argv.index("--") + 1:][0]
OFFSET = 0.25
SEG = 3
PROFILE = 0.5

def cube_bm():
    bpy.ops.object.select_all(action='SELECT')
    bpy.ops.object.delete()
    verts = [(-1,-1,-1),(1,-1,-1),(1,-1,1),(-1,-1,1),(-1,1,-1),(1,1,-1),(1,1,1),(-1,1,1)]
    faces = [(0,1,2,3),(7,6,5,4),(4,5,1,0),(6,7,3,2),(7,4,0,3),(5,6,2,1)]
    mesh = bpy.data.meshes.new("m")
    mesh.from_pydata(verts, [], faces)
    mesh.update()
    obj = bpy.data.objects.new("o", mesh)
    bpy.context.collection.objects.link(obj)
    bpy.context.view_layer.objects.active = obj
    bm = bmesh.new()
    bm.from_mesh(mesh)
    bmesh.ops.recalc_face_normals(bm, faces=bm.faces)
    return mesh, bm

def edge_between(bm, a, b):
    for e in bm.edges:
        p, q = e.verts[0].co, e.verts[1].co
        def near(u, v):
            return all(abs(u[i] - v[i]) < 1e-4 for i in range(3))
        if (near(p, a) and near(q, b)) or (near(p, b) and near(q, a)):
            return e
    raise RuntimeError("edge not found %s %s" % (a, b))

def run(name, edge_keys):
    mesh, bm = cube_bm()
    targets = [edge_between(bm, a, b) for a, b in edge_keys]
    bmesh.ops.bevel(bm, geom=targets, offset=OFFSET, segments=SEG, profile=PROFILE,
                    affect='EDGES', clamp_overlap=False, loop_slide=True)
    bm.to_mesh(mesh)
    nonmani = sum(1 for e in bm.edges if not e.is_manifold)
    vol = bm.calc_volume(signed=True)
    bm.free()
    V = [list(v.co) for v in mesh.vertices]
    F = [list(p.vertices) for p in mesh.polygons]
    json.dump({"offset": OFFSET, "segments": SEG, "profile": PROFILE,
               "nv": len(V), "nf": len(F), "non_manifold": nonmani, "volume": vol,
               "verts": V, "faces": F}, open(prefix + "_" + name + ".json", "w"), indent=1)
    print("K2D3 ORACLE %s: nv=%d nf=%d nonmani=%d vol=%.5f" % (name, len(V), len(F), nonmani, vol))

# A) two adjacent top edges meeting at (1,1,1): (1,1,±1)-(1,1,1)? use edges (1,-1,1)-(1,1,1)? —
#    take the +X+Z edge ((1,-1,1)-(1,1,1)) and the +Y+Z edge ((-1,1,1)-(1,1,1)): share corner (1,1,1).
run("adj2", [((1, -1, 1), (1, 1, 1)), ((-1, 1, 1), (1, 1, 1))])
# B) the 4 edges of the +Z face.
run("loop4", [((-1, -1, 1), (1, -1, 1)), ((1, -1, 1), (1, 1, 1)),
              ((1, 1, 1), (-1, 1, 1)), ((-1, 1, 1), (-1, -1, 1))])
