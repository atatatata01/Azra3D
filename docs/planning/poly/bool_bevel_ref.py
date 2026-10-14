# bool_bevel_ref.py — Blender oracle for boolean SUBTRACT on a BEVELED (dense-loop) mesh.
# Repro of the 2026-07-23 owner report (COMPLETE_BUG_TRACKER.md POLY-BOOL-BEVEL):
# A = long box with rounded top edges (quarter-circle profile, 8 segments = dense thin
#     quad strips exactly like the AZRA bevel output in the screenshot),
# B = plain small box, FLUSH bottom with A (coplanar y=0 — the suspected trigger),
#     overlapping A's right end and protruding in +Z.
# Blender EXACT boolean A − B, dump volume/bbox/nv/nf for the Rust gate
# (topology won't match a BSP result vertex-for-vertex — compare volume + bbox + manifoldness).
#   "C:\Program Files\Blender Foundation\Blender 4.5\blender.exe" --background --python bool_bevel_ref.py -- out.json
import bpy, bmesh, json, math, sys

out = sys.argv[sys.argv.index("--") + 1:][0]

SEG = 8      # bevel segments (dense loops)
R = 0.5      # bevel radius
H = 1.5      # A height
D = 0.75     # A half-depth (z in [-D, D])
L = 6.0      # A length (x in [0, L])


def rounded_profile():
    """(z, y) cross-section, CCW: flat bottom, vertical sides, two rounded top corners."""
    pts = [(-D, 0.0), (D, 0.0), (D, H - R)]
    # right top corner arc: center (D-R, H-R), 0 -> 90 deg
    for i in range(1, SEG + 1):
        a = math.pi / 2 * i / SEG
        pts.append((D - R + R * math.cos(a), H - R + R * math.sin(a)))
    pts.append((-(D - R), H))
    # left top corner arc: center (-(D-R), H-R), 90 -> 180 deg
    for i in range(1, SEG + 1):
        a = math.pi / 2 + math.pi / 2 * i / SEG
        pts.append((-(D - R) + R * math.cos(a), H - R + R * math.sin(a)))
    return pts


def prism_along_x(profile, x0, x1):
    """Extrude a (z,y) profile along X. Returns (verts, faces) with n-gon caps."""
    n = len(profile)
    verts = [(x0, y, z) for (z, y) in profile] + [(x1, y, z) for (z, y) in profile]
    faces = [tuple(range(n - 1, -1, -1)), tuple(range(n, 2 * n))]  # caps (x0 reversed)
    for i in range(n):
        j = (i + 1) % n
        faces.append((i, j, n + j, n + i))
    return verts, faces


def make_obj(name, verts, faces):
    mesh = bpy.data.meshes.new(name)
    mesh.from_pydata(verts, [], faces)
    mesh.update()
    # Hand-built windings are error-prone: force consistent OUTWARD normals so the
    # oracle measures the boolean, not a winding mistake in this script.
    bm = bmesh.new()
    bm.from_mesh(mesh)
    bmesh.ops.recalc_face_normals(bm, faces=bm.faces)
    bm.to_mesh(mesh)
    bm.free()
    obj = bpy.data.objects.new(name, mesh)
    bpy.context.collection.objects.link(obj)
    return obj


def signed_volume(obj):
    bm = bmesh.new()
    bm.from_mesh(obj.data)
    v = bm.calc_volume(signed=True)
    bm.free()
    return v


bpy.ops.object.select_all(action='SELECT')
bpy.ops.object.delete()

va, fa = prism_along_x(rounded_profile(), 0.0, L)
a = make_obj("A", va, fa)

# B: plain box, FLUSH bottom (y=0, coplanar with A's bottom), overlapping A's right end.
bx0, bx1, by0, by1, bz0, bz1 = 3.5, 5.5, 0.0, 1.0, -0.6, 1.5
vb = [(bx0, by0, bz0), (bx1, by0, bz0), (bx1, by0, bz1), (bx0, by0, bz1),
      (bx0, by1, bz0), (bx1, by1, bz0), (bx1, by1, bz1), (bx0, by1, bz1)]
fb = [(3, 2, 1, 0), (4, 5, 6, 7), (0, 1, 5, 4), (2, 3, 7, 6), (3, 0, 4, 7), (1, 2, 6, 5)]
b = make_obj("B", vb, fb)

vol_a, vol_b = signed_volume(a), signed_volume(b)

mod = a.modifiers.new("bool", 'BOOLEAN')
mod.operation = 'DIFFERENCE'
mod.solver = 'EXACT'
mod.object = b
bpy.context.view_layer.objects.active = a
bpy.ops.object.modifier_apply(modifier="bool")

m = a.data
bm = bmesh.new()
bm.from_mesh(m)
vol = bm.calc_volume(signed=True)
nonmani = sum(1 for e in bm.edges if not e.is_manifold)
bm.free()
xs = [v.co.x for v in m.vertices]; ys = [v.co.y for v in m.vertices]; zs = [v.co.z for v in m.vertices]
json.dump({
    "nv": len(m.vertices), "nf": len(m.polygons),
    "vol_a": vol_a, "vol_b": vol_b,
    "volume": vol, "non_manifold_edges": nonmani,
    "bbox_min": [min(xs), min(ys), min(zs)], "bbox_max": [max(xs), max(ys), max(zs)],
}, open(out, "w"), indent=1)
print("BOOL-BEVEL ORACLE: nv=%d nf=%d vol=%.4f nonmani=%d" % (len(m.vertices), len(m.polygons), vol, nonmani))
