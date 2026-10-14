import bpy, bmesh, sys, json, math
argv = sys.argv
outdir = argv[argv.index("--") + 1] if "--" in argv else "."
def clean(): bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
def apply_bool(A, B, op):
    m = A.modifiers.new("b", 'BOOLEAN'); m.operation = op; m.object = B; m.solver='EXACT'
    bpy.context.view_layer.objects.active = A
    bpy.ops.object.modifier_apply(modifier="b")
    bpy.data.objects.remove(B, do_unlink=True)
def report(A, label):
    bm = bmesh.new(); bm.from_mesh(A.data)
    sizes = {}
    tris = 0
    for f in bm.faces:
        sizes[len(f.verts)] = sizes.get(len(f.verts),0)+1
    print(f"[{label}] nv={len(bm.verts)} ne={len(bm.edges)} nf={len(bm.faces)} sizes={sizes}")
    bm.free()
# cube minus cylinder (r=0.5, depth 3 along Z, 16 sides)
clean()
bpy.ops.mesh.primitive_cube_add(size=2, location=(0,0,0))
A = bpy.context.active_object
bpy.ops.mesh.primitive_cylinder_add(radius=0.5, depth=3, vertices=16, location=(0,0,0))
B = bpy.context.active_object
apply_bool(A, B, 'DIFFERENCE'); report(A, "cube-minus-cyl")
# cube minus rotated cube (45deg about Z, offset corner)
clean()
bpy.ops.mesh.primitive_cube_add(size=2, location=(0,0,0))
A = bpy.context.active_object
bpy.ops.mesh.primitive_cube_add(size=2, location=(1.2,0,0), rotation=(0,0,math.radians(45)))
B = bpy.context.active_object
apply_bool(A, B, 'DIFFERENCE'); report(A, "cube-minus-rotcube")
