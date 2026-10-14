"""Oracle: open-top cube (+Z face deleted), bevel ONE vertical edge (x=-1,y=-1, z -1..+1)
whose top endpoint lies ON the open rim. Offset 0.2, profile 0.5, seg 1 and 3.
Run: blender.exe --background --python rim_vertex_bevel_ref.py -- <out_prefix>
"""
import bpy, bmesh, json, sys

out_prefix = sys.argv[sys.argv.index("--") + 1]

def run(seg):
    bpy.ops.wm.read_factory_settings(use_empty=True)
    mesh = bpy.data.meshes.new("m")
    bm = bmesh.new()
    bmesh.ops.create_cube(bm, size=2.0)
    # delete the +Z face -> open top
    top = [f for f in bm.faces if all(v.co.z > 0.999 for v in f.verts)]
    bmesh.ops.delete(bm, geom=top, context='FACES_ONLY')
    # the vertical edge at x=-1,y=-1
    edges = [e for e in bm.edges
             if all(v.co.x < -0.999 and v.co.y < -0.999 for v in e.verts)]
    assert len(edges) == 1, edges
    bmesh.ops.bevel(bm, geom=edges, offset=0.2, offset_type='OFFSET',
                    segments=seg, profile=0.5, affect='EDGES',
                    clamp_overlap=True, loop_slide=True,
                    miter_outer='SHARP', miter_inner='SHARP',
                    vmesh_method='ADJ')
    bm.verts.ensure_lookup_table(); bm.faces.ensure_lookup_table()
    data = {
        "verts": [[round(v.co.x, 6), round(v.co.y, 6), round(v.co.z, 6)] for v in bm.verts],
        "faces": [[v.index for v in f.verts] for f in bm.faces],
        "boundary_edges": sum(1 for e in bm.edges if len(e.link_faces) == 1),
    }
    with open(f"{out_prefix}_seg{seg}.json", "w") as f:
        json.dump(data, f, indent=1)
    print(f"seg{seg}: nv={len(data['verts'])} nf={len(data['faces'])} boundary={data['boundary_edges']}")
    bm.free()

run(1)
run(3)
