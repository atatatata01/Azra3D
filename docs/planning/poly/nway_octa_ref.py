"""N-way miter oracle: octahedron, bevel all 4 edges at the +Y apex.
Identical input mesh to AZRA's `octahedron()` test helper (±1, same 6 verts + 8 tris).
Run: blender.exe --background --python nway_octa_ref.py -- <out.json>
"""
import bpy, bmesh, sys, json

# ---- args ----
argv = sys.argv
out_path = argv[argv.index("--") + 1] if "--" in argv else "nway_octa_ref.json"

# ---- identical octahedron (matches AZRA octahedron(): ±1, 8 outward tris) ----
verts = [
    (0.0, 1.0, 0.0),   # 0 +Y apex
    (1.0, 0.0, 0.0),   # 1 +X
    (0.0, 0.0, 1.0),   # 2 +Z
    (-1.0, 0.0, 0.0),  # 3 -X
    (0.0, 0.0, -1.0),  # 4 -Z
    (0.0, -1.0, 0.0),  # 5 -Y apex
]
faces = [
    (0, 2, 1), (0, 3, 2), (0, 4, 3), (0, 1, 4),  # top fan (+Y)
    (5, 1, 2), (5, 2, 3), (5, 3, 4), (5, 4, 1),  # bottom fan (-Y)
]

bm = bmesh.new()
bmv = [bm.verts.new(v) for v in verts]
bm.verts.index_update()
for f in faces:
    bm.faces.new([bmv[i] for i in f])
bm.normal_update()

# ---- select the 4 edges incident to the apex vertex (index 0, co ~ (0,1,0)) ----
apex = bmv[0]
apex_edges = [e for e in bm.edges if apex in e.verts]
assert len(apex_edges) == 4, f"apex degree = {len(apex_edges)} (expected 4)"

# ---- bevel (match AZRA params: offset width, seg1, circular profile 0.5, sharp miters, clamp, loop slide) ----
res = bmesh.ops.bevel(
    bm,
    geom=apex_edges,
    offset=0.25,
    offset_type='OFFSET',
    segments=1,
    profile=0.5,
    affect='EDGES',
    clamp_overlap=True,
    loop_slide=True,
    miter_outer='SHARP',
    miter_inner='SHARP',
)

bm.verts.index_update()
bm.faces.index_update()
out = {
    "nv": len(bm.verts),
    "nf": len(bm.faces),
    "verts": [[round(v.co.x, 6), round(v.co.y, 6), round(v.co.z, 6)] for v in bm.verts],
    "faces": [[v.index for v in f.verts] for f in bm.faces],
}
with open(out_path, "w") as fh:
    json.dump(out, fh)
print(f"[ORACLE] wrote {out_path}: nv={out['nv']} nf={out['nf']}")
