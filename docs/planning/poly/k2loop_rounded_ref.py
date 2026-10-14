# k2loop_rounded_ref.py — Blender oracle: ROUNDED bevel of ONE edge ending on
# already-chamfered (degree>3) corners — the k=1 partial corner (BEVEL.md §9.4 remainder,
# owner report 2026-07-23: "cannot add round bevel" on a chamfered box).
#
# Single-stage methodology (§9): the chamfered-cube BASE is built EXPLICITLY (same verts in
# the Rust gate — NOT a two-stage bevel whose stage 1 could diverge). Then ONE long edge of
# the +X face (the edge shared with the x=1/y=1 chamfer rect) is beveled rounded.
#   "C:\Program Files\Blender Foundation\Blender 4.5\blender.exe" --background --python k2loop_rounded_ref.py -- out.json
import bpy, bmesh, json, sys

out = sys.argv[sys.argv.index("--") + 1:][0]

T = 0.3          # chamfer width of the base
OFFSET = 0.15    # bevel offset
SEG = 3          # segments (rounded)
PROFILE = 0.5    # circular

s = 1.0 - T

# 24 verts: exactly one coordinate at ±1, the other two at ±s.
verts = []
vid = {}
for axis in range(3):
    for main in (-1.0, 1.0):
        for a in (-s, s):
            for b in (-s, s):
                co = [0.0, 0.0, 0.0]
                co[axis] = main
                co[(axis + 1) % 3] = a
                co[(axis + 2) % 3] = b
                key = tuple(round(c, 6) for c in co)
                if key not in vid:
                    vid[key] = len(verts)
                    verts.append(co)

def k(x, y, z):
    return vid[(round(x, 6), round(y, 6), round(z, 6))]

faces = []
# 6 squares (one per cube face), CCW seen from outside.
faces.append([k(1, -s, -s), k(1, s, -s), k(1, s, s), k(1, -s, s)])       # +X
faces.append([k(-1, -s, -s), k(-1, -s, s), k(-1, s, s), k(-1, s, -s)])   # -X
faces.append([k(-s, 1, -s), k(-s, 1, s), k(s, 1, s), k(s, 1, -s)])       # +Y
faces.append([k(-s, -1, -s), k(s, -1, -s), k(s, -1, s), k(-s, -1, s)])   # -Y
faces.append([k(-s, -s, 1), k(s, -s, 1), k(s, s, 1), k(-s, s, 1)])       # +Z
faces.append([k(-s, -s, -1), k(-s, s, -1), k(s, s, -1), k(s, -s, -1)])   # -Z
# 12 chamfer rects (one per cube edge).
faces.append([k(1, s, -s), k(s, 1, -s), k(s, 1, s), k(1, s, s)])         # +X+Y
faces.append([k(1, -s, s), k(s, -1, s), k(s, -1, -s), k(1, -s, -s)])     # +X-Y
faces.append([k(1, -s, s), k(1, s, s), k(s, s, 1), k(s, -s, 1)])         # +X+Z
faces.append([k(1, s, -s), k(1, -s, -s), k(s, -s, -1), k(s, s, -1)])     # +X-Z
faces.append([k(-1, s, -s), k(-1, s, s), k(-s, 1, s), k(-s, 1, -s)])     # -X+Y
faces.append([k(-1, -s, s), k(-1, -s, -s), k(-s, -1, -s), k(-s, -1, s)]) # -X-Y
faces.append([k(-1, -s, s), k(-s, -s, 1), k(-s, s, 1), k(-1, s, s)])     # -X+Z
faces.append([k(-1, s, -s), k(-s, s, -1), k(-s, -s, -1), k(-1, -s, -s)]) # -X-Z
faces.append([k(s, 1, s), k(-s, 1, s), k(-s, s, 1), k(s, s, 1)])         # +Y+Z
faces.append([k(-s, 1, -s), k(s, 1, -s), k(s, s, -1), k(-s, s, -1)])     # +Y-Z
faces.append([k(-s, -1, s), k(s, -1, s), k(s, -s, 1), k(-s, -s, 1)][::-1]) # -Y+Z
faces.append([k(s, -1, -s), k(-s, -1, -s), k(-s, -s, -1), k(s, -s, -1)][::-1]) # -Y-Z
faces = [f for f in faces if f]
# 8 corner triangles.
for sx in (-1, 1):
    for sy in (-1, 1):
        for sz in (-1, 1):
            tri = [k(sx * 1, sy * s, sz * s), k(sx * s, sy * 1, sz * s), k(sx * s, sy * s, sz * 1)]
            faces.append(tri)

bpy.ops.object.select_all(action='SELECT')
bpy.ops.object.delete()
mesh = bpy.data.meshes.new("chamfered")
mesh.from_pydata(verts, [], faces)
mesh.update()
obj = bpy.data.objects.new("chamfered", mesh)
bpy.context.collection.objects.link(obj)
bpy.context.view_layer.objects.active = obj

bm = bmesh.new()
bm.from_mesh(mesh)
bmesh.ops.recalc_face_normals(bm, faces=bm.faces)  # tolerate winding slips in the hand build

# Target: the FOUR edges of the +Z square face (the top edge LOOP) - each corner is a
# k=2 adjacent pair at a degree-4 chamfer-created vertex (owner loop case).
targets = []
for e in bm.edges:
    a, b = e.verts[0].co, e.verts[1].co
    if abs(a[2] - 1.0) < 1e-4 and abs(b[2] - 1.0) < 1e-4:
        targets.append(e)
assert len(targets) == 4, "want 4 top-square edges, got %d" % len(targets)

bmesh.ops.bevel(bm, geom=targets, offset=OFFSET, segments=SEG, profile=PROFILE,
                affect='EDGES', clamp_overlap=False, loop_slide=True)
bm.to_mesh(mesh)
bm.free()

V = [list(v.co) for v in mesh.vertices]
F = [list(p.vertices) for p in mesh.polygons]
bm2 = bmesh.new(); bm2.from_mesh(mesh)
nonmani = sum(1 for e in bm2.edges if not e.is_manifold)
vol = bm2.calc_volume(signed=True)
bm2.free()
json.dump({"base": {"nv": 24, "t": T}, "offset": OFFSET, "segments": SEG, "profile": PROFILE,
           "nv": len(V), "nf": len(F), "non_manifold": nonmani, "volume": vol,
           "verts": V, "faces": F}, open(out, "w"), indent=1)
print("K1-ROUNDED ORACLE: nv=%d nf=%d nonmani=%d vol=%.5f" % (len(V), len(F), nonmani, vol))
