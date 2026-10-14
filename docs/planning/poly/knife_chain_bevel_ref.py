"""Knife-chain bevel oracle: cube with a 3-segment knife cut across the front face
(T-junction terminals on the vertical box edges, one bend vertex mid-face), bevel the
3 chain edges. This is the owner's live repro (AZRA produced non-manifold output:
duplicated cap-fan faces at the valence-2 bend vertex and the T terminals).

Topology BEFORE bevel — identical to AZRA's knife output (12 verts, 7 faces):
  box corners 0..7; chain verts 8=(-1,-0.2,1) [T on left edge], 9=(-0.3,-0.2,1) [bend],
  10=(0.3,0.35,1) [bend], 11=(1,0.35,1) [T on right edge].
  Front face split into two n-gons along 8-9-10-11; left/right side faces become
  pentagons (T verts inserted); bottom/top/back unchanged.

Run: blender.exe --background --python knife_chain_bevel_ref.py -- <out.json> <segments>
"""
import bpy, bmesh, sys, json

argv = sys.argv
args = argv[argv.index("--") + 1:] if "--" in argv else []
out_path = args[0] if len(args) > 0 else "knife_chain_bevel_ref.json"
segments = int(args[1]) if len(args) > 1 else 1

verts = [
    (1.0, -1.0, 1.0),    # 0
    (-1.0, -1.0, 1.0),   # 1
    (-1.0, -1.0, -1.0),  # 2
    (1.0, -1.0, -1.0),   # 3
    (1.0, 1.0, -1.0),    # 4
    (-1.0, 1.0, -1.0),   # 5
    (-1.0, 1.0, 1.0),    # 6
    (1.0, 1.0, 1.0),     # 7
    (-1.0, -0.2, 1.0),   # 8  T-terminal on the left vertical edge (1-6 span)
    (-0.3, -0.2, 1.0),   # 9  bend (valence 2)
    (0.3, 0.35, 1.0),    # 10 bend (valence 2)
    (1.0, 0.35, 1.0),    # 11 T-terminal on the right vertical edge (0-7 span)
]
faces = [
    (0, 1, 2, 3),                 # bottom
    (4, 5, 6, 7),                 # top
    (3, 2, 5, 4),                 # back
    (1, 0, 11, 10, 9, 8),         # front lower n-gon
    (7, 6, 8, 9, 10, 11),         # front upper n-gon
    (2, 1, 8, 6, 5),              # left side pentagon (T vert 8 inserted)
    (11, 0, 3, 4, 7),             # right side pentagon (T vert 11 inserted)
]

bm = bmesh.new()
bmv = [bm.verts.new(v) for v in verts]
bm.verts.index_update()
for f in faces:
    bm.faces.new([bmv[i] for i in f])
bm.normal_update()

# The 3 chain edges: 8-9, 9-10, 10-11.
chain = []
want = {(8, 9), (9, 10), (10, 11)}
for e in bm.edges:
    key = tuple(sorted((e.verts[0].index, e.verts[1].index)))
    if key in want:
        chain.append(e)
assert len(chain) == 3, f"chain edges = {len(chain)}"

bmesh.ops.bevel(
    bm, geom=chain, offset=0.27, offset_type='OFFSET', segments=segments,
    profile=0.5, affect='EDGES', clamp_overlap=True, loop_slide=True,
    miter_outer='SHARP', miter_inner='SHARP',
)

bm.verts.index_update(); bm.faces.index_update()

# Manifold check: every edge must have exactly 1 or 2 link_faces, and no directed dup.
nonman = [e.index for e in bm.edges if len(e.link_faces) > 2]
out = {
    "nv": len(bm.verts), "nf": len(bm.faces),
    "nonmanifold_edges": nonman,
    "verts": [[round(v.co.x, 6), round(v.co.y, 6), round(v.co.z, 6)] for v in bm.verts],
    "faces": [[v.index for v in f.verts] for f in bm.faces],
}
with open(out_path, "w") as fh:
    json.dump(out, fh)
print(f"[ORACLE] wrote {out_path}: nv={out['nv']} nf={out['nf']} nonmanifold={nonman}")
# Eyeball: verts near the bend (9) and terminal (8)
near9 = [[round(v.co.x,4),round(v.co.y,4),round(v.co.z,4)] for v in bm.verts
         if abs(v.co.x + 0.3) < 0.45 and abs(v.co.y + 0.2) < 0.45 and v.co.z > 0.9]
print(f"[ORACLE] verts near bend 9: {near9}")
near8 = [[round(v.co.x,4),round(v.co.y,4),round(v.co.z,4)] for v in bm.verts
         if v.co.x < -0.9 and abs(v.co.y + 0.2) < 0.5]
print(f"[ORACLE] verts near T-terminal 8: {near8}")
