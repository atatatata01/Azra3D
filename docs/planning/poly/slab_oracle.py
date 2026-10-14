import bpy, json, sys
from collections import Counter
out = sys.argv[sys.argv.index("--")+1:][0]
bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0,0,0)); A=bpy.context.active_object
# cutter = slab spanning full X, y in [0,2], z in [0,2] -> removes [-1,1]x[0,1]x[0,1]
bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0,1,1)); C=bpy.context.active_object
C.scale=(2,1,1); bpy.ops.object.transform_apply(scale=True)
bpy.context.view_layer.objects.active=A
m=A.modifiers.new("b",'BOOLEAN'); m.operation='DIFFERENCE'; m.object=C; m.solver='EXACT'
bpy.ops.object.modifier_apply(modifier="b")
mesh=A.data
V=[list(v.co) for v in mesh.vertices]; F=[list(p.vertices) for p in mesh.polygons]
sz={}
for f in F: sz[len(f)]=sz.get(len(f),0)+1
print("SLAB:", {"nv":len(V),"nf":len(F),"face_sizes":sz})
