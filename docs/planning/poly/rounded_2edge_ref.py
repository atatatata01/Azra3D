"""Rounded 2-edge miter oracle: square pyramid, bevel EXACTLY 2 OPPOSITE apex edges, segments=3.

Identical input mesh to AZRA's `square_pyramid()` (apex degree-4, base corners degree-3).
This isolates a ROUNDED (seg>1) 2-edge miter at a degree>3 vertex — the M_NONE weld
(the two edges' profile arcs co-planarized onto ONE plane + bridged by per-segment SHARED
interior verts, per BEVEL.md §3.7). No corner grid.

The 2 beveled edges are apex->v2 and apex->v4 (OPPOSITE apex edges: they do NOT share a face).
Each of the two miter sectors has 2 between-edges (the non-beveled apex->v1 / apex->v3), so the
flat BoundVert is a loop-slide meet (between==2) — which AZRA already matches Blender exactly.
Rounding it (segments>1) bridges the two BoundVerts with the co-planarized weld profile arc.

Run: blender.exe --background --python rounded_2edge_ref.py -- <out.json>
"""
import bpy, bmesh, sys, json

argv = sys.argv
out_path = argv[argv.index("--") + 1] if "--" in argv else "rounded_2edge_ref.json"

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
# EXACTLY 2 OPPOSITE apex edges: apex->v2 and apex->v4 (do NOT share a face).
sel_targets = {2, 4}
sel_edges = [e for e in bm.edges if apex in e.verts and
             (e.other_vert(apex).index in sel_targets)]
assert len(sel_edges) == 2, f"selected {len(sel_edges)} edges (expected 2)"

bmesh.ops.bevel(
    bm, geom=sel_edges, offset=0.25, offset_type='OFFSET', segments=3,
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
# apex-region verts (y > 0.2) = the rounded miter arc chain (2 BoundVerts + ns-1 shared interior verts)
apex_arc = [[round(v.co.x, 4), round(v.co.y, 4), round(v.co.z, 4)] for v in bm.verts if v.co.y > 0.2]
print(f"[ORACLE] miter arc verts (y>0.2), count={len(apex_arc)}: {apex_arc}")
