# bridge_multi_ref.py — Blender oracle for BRIDGE EDGE LOOPS with N > 2 loops.
#   blender.exe --background --python bridge_multi_ref.py -- out.json
#
# Records algorithm-INDEPENDENT invariants only (tessellation differs from ours):
#   nv / nf, face-arity histogram, boundary-edge count, per-edge face-count census,
#   is_manifold, volume. The question this oracle answers is:
#     with N open rings selected in ONE invocation, how many of them does Blender bridge?
#   (chain of N-1 bridges? only the first pair? per-island pairing?)
import bpy, bmesh, json, sys, math

out = sys.argv[sys.argv.index("--") + 1:][0]


def fresh(verts, faces):
    bpy.ops.object.select_all(action='SELECT')
    bpy.ops.object.delete()
    mesh = bpy.data.meshes.new("m")
    mesh.from_pydata(verts, [], faces)
    mesh.update()
    obj = bpy.data.objects.new("o", mesh)
    bpy.context.collection.objects.link(obj)
    bpy.context.view_layer.objects.active = obj
    return mesh, obj


def ring(n, y, r=1.0, phase=0.0):
    return [(round(r * math.cos(2 * math.pi * k / n + phase), 6), y,
             round(r * math.sin(2 * math.pi * k / n + phase), 6)) for k in range(n)]


def census(mesh):
    bm = bmesh.new()
    bm.from_mesh(mesh)
    bm.edges.ensure_lookup_table()
    edge_fc = {}
    for e in bm.edges:
        edge_fc[len(e.link_faces)] = edge_fc.get(len(e.link_faces), 0) + 1
    boundary = sum(1 for e in bm.edges if len(e.link_faces) == 1)
    nonmanifold = sum(1 for e in bm.edges if len(e.link_faces) > 2)
    try:
        vol = round(bm.calc_volume(signed=False), 6)
    except Exception:
        vol = None
    arity = {}
    for f in bm.faces:
        arity[len(f.verts)] = arity.get(len(f.verts), 0) + 1
    r = {"nv": len(bm.verts), "ne": len(bm.edges), "nf": len(bm.faces),
         "boundary_edges": boundary, "nonmanifold_edges": nonmanifold,
         "edge_face_census": {str(k): v for k, v in sorted(edge_fc.items())},
         "face_arity": {str(k): v for k, v in sorted(arity.items())},
         "volume": vol}
    bm.free()
    return r


def bridge_boundary_rings(mesh, want_rings):
    """Select ALL boundary edges belonging to the first `want_rings` rings and bridge once."""
    bpy.ops.object.mode_set(mode='EDIT')
    bpy.ops.mesh.select_mode(type='EDGE')
    bpy.ops.mesh.select_all(action='DESELECT')
    bm = bmesh.from_edit_mesh(mesh)
    bm.edges.ensure_lookup_table()
    picked = 0
    for e in bm.edges:
        if len(e.link_faces) <= 1:
            e.select = True
            picked += 1
    bmesh.update_edit_mesh(mesh)
    before = picked
    try:
        bpy.ops.mesh.bridge_edge_loops()
        err = None
    except Exception as ex:
        err = str(ex)
    bpy.ops.object.mode_set(mode='OBJECT')
    c = census(mesh)
    c["selected_edges"] = before
    c["error"] = err
    return c


res = {}

# ---------------------------------------------------------------- case 1: 3 coaxial open rings
# Three free-standing 8-gon wire rings? Blender needs FACES to have boundary edges, so build
# each ring as a filled ngon cap, then delete the caps -> pure wire rings are not selectable as
# boundary. Instead: three short tubes stacked with gaps, each contributing 2 boundary rings.
# Simpler and closer to the user's case: 3 open ngon caps (each an n-gon face). Its boundary is
# the ring. Bridging N such rings is exactly "3 loops selected at once".
def ngon_stack(ys, n=8, r=1.0):
    verts, faces = [], []
    for y in ys:
        base = len(verts)
        verts += ring(n, y, r)
        faces.append(tuple(range(base, base + n)))
    return verts, faces

v, f = ngon_stack([0.0, 1.0, 2.0])
m, _ = fresh(v, f)
res["three_ngon_rings"] = bridge_boundary_rings(m, 3)

# ---------------------------------------------------------------- case 2: 4 coaxial open rings
v, f = ngon_stack([0.0, 1.0, 2.0, 3.0])
m, _ = fresh(v, f)
res["four_ngon_rings"] = bridge_boundary_rings(m, 4)

# ---------------------------------------------------------------- case 3: baseline, 2 rings
v, f = ngon_stack([0.0, 1.0])
m, _ = fresh(v, f)
res["two_ngon_rings"] = bridge_boundary_rings(m, 2)

# ---------------------------------------------------------------- case 4: unequal counts, 3 rings
verts, faces = [], []
for (y, n) in [(0.0, 6), (1.0, 8), (2.0, 4)]:
    base = len(verts)
    verts += ring(n, y)
    faces.append(tuple(range(base, base + n)))
m, _ = fresh(verts, faces)
res["three_unequal_rings"] = bridge_boundary_rings(m, 3)

# ---------------------------------------------------------------- case 5: two ISLANDS, 2 rings each
# Island A at x=-4, island B at x=+4. Four boundary rings total, two disjoint components.
verts, faces = [], []
for dx in (-4.0, 4.0):
    for y in (0.0, 1.0):
        base = len(verts)
        verts += [(x + dx, yy, z) for (x, yy, z) in ring(8, y)]
        faces.append(tuple(range(base, base + 8)))
m, _ = fresh(verts, faces)
res["two_islands_two_rings_each"] = bridge_boundary_rings(m, 4)

# ---------------------------------------------------------------- case 6: WIRE rings, no caps
# The manifold-clean N-loop case: three free-standing 8-gon wire rings, nothing else. Every
# selected edge starts with 0 faces, so a chain of bridges cannot make anything non-manifold.
# This is the case AZRA's app-level is_manifold gate must ACCEPT.
def wire_rings(ys, n=8, r=1.0):
    verts, edges = [], []
    for y in ys:
        base = len(verts)
        verts += ring(n, y, r)
        edges += [(base + k, base + (k + 1) % n) for k in range(n)]
    return verts, edges


def bridge_wire(verts, edges):
    bpy.ops.object.select_all(action='SELECT')
    bpy.ops.object.delete()
    mesh = bpy.data.meshes.new("m")
    mesh.from_pydata(verts, edges, [])
    mesh.update()
    obj = bpy.data.objects.new("o", mesh)
    bpy.context.collection.objects.link(obj)
    bpy.context.view_layer.objects.active = obj
    bpy.ops.object.mode_set(mode='EDIT')
    bpy.ops.mesh.select_mode(type='EDGE')
    bpy.ops.mesh.select_all(action='SELECT')
    n_sel = len(edges)
    try:
        bpy.ops.mesh.bridge_edge_loops()
        err = None
    except Exception as ex:
        err = str(ex)
    bpy.ops.object.mode_set(mode='OBJECT')
    c = census(mesh)
    c["selected_edges"] = n_sel
    c["error"] = err
    return c


v, e = wire_rings([0.0, 1.0, 2.0])
res["three_wire_rings"] = bridge_wire(v, e)
v, e = wire_rings([0.0, 1.0, 2.0, 3.0])
res["four_wire_rings"] = bridge_wire(v, e)

# case 7: wire rings, UNEQUAL counts (6 / 8 / 4)
verts, edges = [], []
for (y, n) in [(0.0, 6), (1.0, 8), (2.0, 4)]:
    base = len(verts)
    verts += ring(n, y)
    edges += [(base + k, base + (k + 1) % n) for k in range(n)]
res["three_wire_unequal"] = bridge_wire(verts, edges)

# case 8: TWO ISLANDS of wire rings, 2 rings each (x = -4 and x = +4)
verts, edges = [], []
for dx in (-4.0, 4.0):
    for y in (0.0, 1.0):
        base = len(verts)
        verts += [(x + dx, yy, z) for (x, yy, z) in ring(8, y)]
        edges += [(base + k, base + (k + 1) % 8) for k in range(8)]
res["two_islands_wire"] = bridge_wire(verts, edges)

json.dump(res, open(out, "w"), indent=1)
for k, c in res.items():
    print("BRIDGE %-26s sel=%-3s nv=%-4d ne=%-4d nf=%-4d bnd=%-4d nonman=%d arity=%s err=%s"
          % (k, c["selected_edges"], c["nv"], c["ne"], c["nf"],
             c["boundary_edges"], c["nonmanifold_edges"], c["face_arity"], c["error"]))
