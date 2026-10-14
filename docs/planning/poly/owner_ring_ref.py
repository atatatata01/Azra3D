# owner_ring_ref.py - Blender oracle for the owner's EXACT scene (base.fl4 30v/28f):
# bevel the full z=0.5 ring (10 edges, crossing a PRIOR rounded fillet), offset .242 seg 3.
import bpy, bmesh, json, sys
out = sys.argv[sys.argv.index('--') + 1:][0]
verts = [(-0.78762376, -0.5, 0.5), (-0.34763402, -0.5, -0.5), (-0.34763402, 0.5, -0.5), (-0.78762376, 0.5, 0.5), (-0.3464583, 0.5, 1.4214342), (-0.3464583, -0.5, 1.4214342), (-0.94813156, 0.35460013, 0.5), (-0.55946076, 0.35460013, -0.3673141), (-0.55946076, -0.35460013, -0.36731413), (-0.94813156, -0.35460013, 0.5), (-0.56359214, -0.35460013, 1.290678), (-0.56359214, 0.35460013, 1.290678), (-0.86787766, -0.48052013, 0.5), (-0.92662764, -0.42730007, 0.5), (-0.43178323, -0.48052013, -0.48222345), (-0.5093172, -0.42730007, -0.43365708), (-0.43178323, 0.48052013, -0.48222345), (-0.5093172, 0.42730007, -0.43365705), (-0.86787766, 0.48052013, 0.5), (-0.9266276, 0.42730007, 0.5), (-0.4317489, 0.48052013, 1.4039161), (-0.5112254, 0.42730007, 1.3560561), (-0.43174887, -0.48052013, 1.4039161), (-0.5112254, -0.42730007, 1.3560561), (0.5, -0.5, -0.5), (0.5, -0.5, 0.5), (0.5, 0.5, 0.5), (0.5, 0.5, -0.5), (0.5, 0.5, 1.4214342), (0.5, -0.5, 1.4214342)]
faces = [(24, 25, 0, 1), (26, 27, 2, 3), (27, 24, 1, 2), (4, 5, 29, 28), (7, 8, 9, 6), (25, 24, 27, 26), (3, 4, 28, 26), (9, 10, 11, 6), (25, 29, 5, 0), (28, 29, 25, 26), (0, 12, 14, 1), (12, 13, 15, 14), (13, 9, 8, 15), (2, 16, 18, 3), (16, 17, 19, 18), (17, 7, 6, 19), (1, 14, 16, 2), (14, 15, 17, 16), (15, 8, 7, 17), (4, 20, 22, 5), (20, 21, 23, 22), (21, 11, 10, 23), (3, 18, 20, 4), (18, 19, 21, 20), (19, 6, 11, 21), (9, 13, 23, 10), (13, 12, 22, 23), (12, 0, 5, 22)]
bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
mesh = bpy.data.meshes.new('m'); mesh.from_pydata(verts, [], faces); mesh.update()
obj = bpy.data.objects.new('o', mesh); bpy.context.collection.objects.link(obj)
bpy.context.view_layer.objects.active = obj
bm = bmesh.new(); bm.from_mesh(mesh)
bmesh.ops.recalc_face_normals(bm, faces=bm.faces)
targets = [e for e in bm.edges if abs(e.verts[0].co[2] - 0.5) < 1e-3 and abs(e.verts[1].co[2] - 0.5) < 1e-3]
print('selected', len(targets))
assert len(targets) == 10, 'want 10 ring edges got %d' % len(targets)
bmesh.ops.bevel(bm, geom=targets, offset=0.242, segments=3, profile=0.5, affect='EDGES', clamp_overlap=False, loop_slide=True)
bm.to_mesh(mesh)
nonmani = sum(1 for e in bm.edges if not e.is_manifold)
bm.free()
Vo = [list(v.co) for v in mesh.vertices]
Fo = [list(p.vertices) for p in mesh.polygons]
json.dump({'nv': len(Vo), 'nf': len(Fo), 'non_manifold': nonmani, 'verts': Vo, 'faces': Fo}, open(out, 'w'), indent=1)
print('OWNER-RING ORACLE: nv=%d nf=%d nonmani=%d' % (len(Vo), len(Fo), nonmani))
