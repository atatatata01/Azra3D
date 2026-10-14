"""Knife-chain bevel oracle #2 — the owner's corner case: a 2-wide box (middle loop at x=0),
knife chain on the FRONT-RIGHT quad: T1 on the BOTTOM edge (oblique diagonal), a bend, then a
horizontal segment ending T2 ON the RIGHT CORNER edge (front/right dihedral). Bevel the 2 chain
edges. AZRA breaks here (strip pokes through near the oblique bottom terminal); Blender is clean.

Topology BEFORE bevel (14 verts, 12 faces), mirroring AZRA's knife output exactly:
  outer corners x=±1, middle loop x=0, y/z=±1.
  T1 = (0.3, -1, 1) on the bottom edge of the front-right quad (shared with bottom-right face)
  bend = (0.55, -0.45, 1) inside the front-right quad
  T2 = (1, -0.45, 1) on the corner edge (shared with the right face)

Run: blender.exe --background --factory-startup --python knife_corner_bevel_ref.py -- <out.json> <segments>
"""
import bpy, bmesh, sys, json

argv = sys.argv
args = argv[argv.index("--") + 1:] if "--" in argv else []
out_path = args[0] if len(args) > 0 else "knife_corner_ref.json"
segments = int(args[1]) if len(args) > 1 else 1

verts = [
    (-1.0, -1.0, 1.0),   # 0
    (0.0, -1.0, 1.0),    # 1  middle front-bottom
    (1.0, -1.0, 1.0),    # 2
    (1.0, 1.0, 1.0),     # 3
    (0.0, 1.0, 1.0),     # 4  middle front-top
    (-1.0, 1.0, 1.0),    # 5
    (-1.0, -1.0, -1.0),  # 6
    (0.0, -1.0, -1.0),   # 7  middle back-bottom
    (1.0, -1.0, -1.0),   # 8
    (1.0, 1.0, -1.0),    # 9
    (0.0, 1.0, -1.0),    # 10 middle back-top
    (-1.0, 1.0, -1.0),   # 11
    (0.3, -1.0, 1.0),    # 12 T1 (bottom edge of front-right)
    (0.55, -0.45, 1.0),  # 13 bend
    (1.0, -0.45, 1.0),   # 14 T2 (corner edge front/right)
]
faces = [
    (0, 1, 4, 5),                    # front-left
    (1, 12, 13, 14, 3, 4),           # front-right upper (chain absorbed)
    (12, 2, 14, 13),                 # front-right lower (chain absorbed)
    (5, 4, 10, 11),                  # top-left
    (4, 3, 9, 10),                   # top-right
    (7, 6, 11, 10),                  # back-left
    (8, 7, 10, 9),                   # back-right
    (0, 6, 7, 1),                    # bottom-left
    (1, 7, 8, 2, 12),                # bottom-right (T1 inserted)
    (0, 5, 11, 6),                   # left
    (2, 8, 9, 3, 14),                # right (T2 inserted)
]

bm = bmesh.new()
bmv = [bm.verts.new(v) for v in verts]
bm.verts.index_update()
for f in faces:
    bm.faces.new([bmv[i] for i in f])
bm.normal_update()
# Fix any inconsistent windings from hand-entry
bmesh.ops.recalc_face_normals(bm, faces=bm.faces[:])

chain = []
want = {(12, 13), (13, 14)}
for e in bm.edges:
    key = tuple(sorted((e.verts[0].index, e.verts[1].index)))
    if key in want:
        chain.append(e)
assert len(chain) == 2, f"chain edges = {len(chain)}"

bmesh.ops.bevel(
    bm, geom=chain, offset=0.15, offset_type='OFFSET', segments=segments,
    profile=0.5, affect='EDGES', clamp_overlap=True, loop_slide=True,
    miter_outer='SHARP', miter_inner='SHARP',
)

bm.verts.index_update(); bm.faces.index_update()
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
near_t1 = [[round(v.co.x,4),round(v.co.y,4),round(v.co.z,4)] for v in bm.verts
           if abs(v.co.y + 1.0) < 0.35 and 0.0 < v.co.x < 0.75 and v.co.z > 0.9]
print(f"[ORACLE] verts near T1 (bottom, oblique): {near_t1}")
near_t2 = [[round(v.co.x,4),round(v.co.y,4),round(v.co.z,4)] for v in bm.verts
           if v.co.x > 0.9 and abs(v.co.y + 0.45) < 0.35]
print(f"[ORACLE] verts near T2 (corner edge): {near_t2}")
