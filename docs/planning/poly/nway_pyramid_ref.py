"""N-way miter oracle: square pyramid, bevel all 4 apex edges.
Identical input to AZRA's `square_pyramid()` (apex degree-4, base corners degree-3).
Run: blender.exe --background --python nway_pyramid_ref.py -- <out.json>
"""
import bpy, bmesh, sys, json

argv = sys.argv
out_path = argv[argv.index("--") + 1] if "--" in argv else "nway_pyramid_ref.json"

verts = [
    (0.0, 1.0, 0.0),     # 0 apex (degree-4)
    (1.0, -1.0, 1.0),    # 1
    (-1.0, -1.0, 1.0),   # 2
    (-1.0, -1.0, -1.0),  # 3
    (1.0, -1.0, -1.0),   # 4
]
faces = [
    (0, 2, 1), (0, 3, 2), (0, 4, 3), (0, 1, 4),  # 4 side tris
    (1, 2, 3, 4),                                 # base quad
]

bm = bmesh.new()
bmv = [bm.verts.new(v) for v in verts]
bm.verts.index_update()
for f in faces:
    bm.faces.new([bmv[i] for i in f])
bm.normal_update()

apex = bmv[0]
apex_edges = [e for e in bm.edges if apex in e.verts]
assert len(apex_edges) == 4, f"apex degree = {len(apex_edges)}"

bmesh.ops.bevel(
    bm, geom=apex_edges, offset=0.25, offset_type='OFFSET', segments=1,
    profile=0.5, affect='EDGES', clamp_overlap=True, loop_slide=True,
    miter_outer='SHARP', miter_inner='SHARP',
)

bm.verts.index_update(); bm.faces.index_update()
out = {
    "nv": len(bm.verts), "nf": len(bm.faces),
    "verts": [[round(v.co.x, 6), round(v.co.y, 6), round(v.co.z, 6)] for v in bm.verts],
    "faces": [[v.index for v in f.verts] for f in bm.faces],
}
with open(out_path, "w") as fh:
    json.dump(out, fh)
print(f"[ORACLE] wrote {out_path}: nv={out['nv']} nf={out['nf']}")
# print the apex-cap verts (y > 0.3) for quick eyeball
apex_ring = [[round(v.co.x,4),round(v.co.y,4),round(v.co.z,4)] for v in bm.verts if v.co.y > 0.3]
print(f"[ORACLE] apex-ring verts (y>0.3): {apex_ring}")
