"""Inner-miter ARC oracle — BEVEL.md §3.8 follow-up.

The §3.8 study left ONE thing undetermined: how the innermost arc (S0..S1) attaches to the
two adjoining WALL faces (the "7:2" arity note). This dumps the FULL corner neighbourhood —
every face touching any corner vertex — under Sharp and Arc, so the attachment is measured,
not inferred.

Scenario: boss-on-plate, concave corner at (0.6, 0.6, 1.0). w=0.1382, seg=3, profile 0.5,
spread=0.1 — identical params to §3.8.
"""
import bpy, bmesh, json, sys
from mathutils import Vector

W, SEG, PROF, SPREAD = 0.1382, 3, 0.5, 0.1
CORNER = Vector((0.6, 0.6, 1.0))


def fresh():
    bpy.ops.wm.read_factory_settings(use_empty=True)


def build_base():
    """Plate [-1,1]x[-1,1]x[0,1] with a boss [-0.6,0.6]^2 x [1,2] welded on top."""
    bpy.ops.mesh.primitive_cube_add(size=2)
    plate = bpy.context.object
    plate.scale = (1, 1, 0.5)
    plate.location = (0, 0, 0.5)
    bpy.ops.object.transform_apply(scale=True, location=True)

    bpy.ops.mesh.primitive_cube_add(size=2)
    boss = bpy.context.object
    boss.scale = (0.6, 0.6, 0.5)
    boss.location = (0, 0, 1.5)
    bpy.ops.object.transform_apply(scale=True, location=True)

    m = plate.modifiers.new("bool", 'BOOLEAN')
    m.operation = 'UNION'
    m.object = boss
    m.solver = 'EXACT'
    bpy.context.view_layer.objects.active = plate
    bpy.ops.object.modifier_apply(modifier="bool")
    bpy.data.objects.remove(boss, do_unlink=True)
    return plate


def rim_edges(bm):
    """The four boss-base rim edges: both endpoints at z==1.0 and |x|,|y| <= 0.6+eps."""
    out = []
    for e in bm.edges:
        a, b = e.verts[0].co, e.verts[1].co
        if abs(a.z - 1.0) < 1e-5 and abs(b.z - 1.0) < 1e-5:
            if max(abs(a.x), abs(a.y), abs(b.x), abs(b.y)) < 0.6 + 1e-5:
                out.append(e)
    return out


def run(miter_inner):
    fresh()
    obj = build_base()
    bm = bmesh.new()
    bm.from_mesh(obj.data)
    bm.normal_update()
    sel = rim_edges(bm)
    assert len(sel) == 4, f"expected 4 rim edges, got {len(sel)}"
    bmesh.ops.bevel(
        bm, geom=sel, offset=W, segments=SEG, profile=PROF,
        affect='EDGES', offset_type='OFFSET', clamp_overlap=True,
        miter_inner=miter_inner, spread=SPREAD,
    )
    bm.normal_update()

    # Everything within 0.45 of the corner — the whole neighbourhood, walls included.
    near = [v for v in bm.verts if (v.co - CORNER).length < 0.45]
    idx = {v.index: i for i, v in enumerate(near)}
    verts = [[round(c, 5) for c in v.co] for v in near]
    faces = []
    for f in bm.faces:
        if any(v.index in idx for v in f.verts):
            faces.append({
                "n": len(f.verts),
                # -1 marks a vertex outside the neighbourhood (keeps arity honest)
                "loop": [idx.get(v.index, -1) for v in f.verts],
                "pos": [[round(c, 5) for c in v.co] for v in f.verts],
            })
    bm.free()
    return {"mode": miter_inner, "nv_near": len(near), "verts": verts, "faces": faces}


if __name__ == "__main__":
    out = {m: run(m) for m in ('SHARP', 'ARC')}
    path = sys.argv[-1]
    with open(path, 'w') as fh:
        json.dump(out, fh, indent=1)
    print("WROTE", path)
