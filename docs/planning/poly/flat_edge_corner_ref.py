# flat_edge_corner_ref.py — Blender oracle for the FLAT-UNBEVELED-SECTOR corner.
#
# Owner report 2026-08-15: "bevel bug, the edge from the boolean creates 4 edges that break
# the rounded cap". A boolean leaves a COPLANAR support edge running across a flat face out
# to a box corner. That corner is then degree-4 with 3 beveled edges + 1 FLAT unbeveled one,
# and AZRA's rounded corner comes out broken.
#
# SINGLE-STAGE fixture (BEVEL.md §9 methodology — never a two-stage bevel, the stage-1 base
# would diverge): a plain cube whose TOP face carries one diagonal edge 5-7. That diagonal is
# exactly a boolean support edge in miniature — coplanar, dihedral 0, running corner to corner.
# Verts 5 (1,-1,1) and 7 (-1,1,1) become the degree-4 corners under test; 4 and 6 stay degree-3
# and are the CONTROL in the same mesh.
#
# SX stretches the box along X. At SX=1 the cube's symmetry puts the corner's offset_meet
# EXACTLY on the diagonal, so a loop-slide onto the flat edge and a plain rail meet coincide and
# the fixture cannot tell them apart. SX=2 separates them and is the general case.
#
#   "C:\Program Files\Blender Foundation\Blender 4.5\blender.exe" --background \
#       --python flat_edge_corner_ref.py -- out.json [width] [segments] [sx]
import bpy, bmesh, json, sys

argv = sys.argv[sys.argv.index("--") + 1:]
out = argv[0]
WIDTH = float(argv[1]) if len(argv) > 1 else 0.1
SEG = int(argv[2]) if len(argv) > 2 else 3
SX = float(argv[3]) if len(argv) > 3 else 1.0

V = [(-SX, -1, -1), (SX, -1, -1), (SX, 1, -1), (-SX, 1, -1),
     (-SX, -1, 1), (SX, -1, 1), (SX, 1, 1), (-SX, 1, 1)]
F = [(0, 3, 2, 1),            # bottom
     (4, 5, 7), (5, 6, 7),    # top, SPLIT by the flat diagonal 5-7
     (0, 1, 5, 4), (1, 2, 6, 5), (2, 3, 7, 6), (3, 0, 4, 7)]

bpy.ops.object.select_all(action='SELECT')
bpy.ops.object.delete()

mesh = bpy.data.meshes.new("base")
mesh.from_pydata(V, [], F)
mesh.update()
obj = bpy.data.objects.new("base", mesh)
bpy.context.collection.objects.link(obj)
bpy.context.view_layer.objects.active = obj

bm = bmesh.new()
bm.from_mesh(mesh)
bm.verts.ensure_lookup_table()
bm.edges.ensure_lookup_table()

# The 12 CUBE edges = endpoints differing in exactly one coordinate. The diagonal 5-7 differs
# in two, so it is left unselected -- the boolean support edge the owner cannot bevel.
sel = []
for e in bm.edges:
    a, b = e.verts[0].co, e.verts[1].co
    diff = sum(1 for i in range(3) if abs(a[i] - b[i]) > 1e-6)
    if diff == 1:
        sel.append(e)
assert len(sel) == 12, "expected the 12 cube edges, got %d" % len(sel)

base = {"nv": len(bm.verts), "nf": len(bm.faces),
        "verts": [[round(c, 7) for c in v.co] for v in bm.verts],
        "faces": [[v.index for v in f.verts] for f in bm.faces]}

bmesh.ops.bevel(
    bm, geom=sel, offset=WIDTH, offset_type='OFFSET', segments=SEG, profile=0.5,
    affect='EDGES', clamp_overlap=True, loop_slide=True,
    miter_outer='SHARP', miter_inner='SHARP', vmesh_method='ADJ',
)
bm.normal_update()

nonmani = sum(1 for e in bm.edges if not e.is_manifold)
verts = [[round(c, 7) for c in v.co] for v in bm.verts]
faces = [[v.index for v in f.verts] for f in bm.faces]

# Per-corner report: what fills the corner of the flat-edge vertex vs the clean control.
def local(c, r=0.32):
    vs = [i for i, p in enumerate(verts)
          if sum((p[k] - c[k]) ** 2 for k in range(3)) < r * r]
    s = set(vs)
    fs = [f for f in faces if all(v in s for v in f)]
    return {"verts": len(vs), "faces": len(fs), "sizes": sorted(len(f) for f in fs)}

report = {
    "width": WIDTH, "segments": SEG, "sx": SX,
    "base": base,
    "nv": len(bm.verts), "nf": len(bm.faces), "non_manifold_edges": nonmani,
    "verts": verts, "faces": faces,
    "corner_flat_1": local((SX, -1, 1)),     # degree-4, diagonal lands here
    "corner_flat_2": local((-SX, 1, 1)),     # degree-4, diagonal lands here
    "corner_clean_1": local((-SX, -1, 1)),   # degree-3 control, same mesh
    "corner_clean_2": local((SX, 1, 1)),     # degree-3 control, same mesh
}
bm.free()
json.dump(report, open(out, "w"), indent=1)
print("FLAT-EDGE CORNER ORACLE: nv=%d nf=%d nonmani=%d" % (report["nv"], report["nf"], nonmani))
print("  flat corner   :", report["corner_flat_1"])
print("  clean control :", report["corner_clean_1"])
