"""Boolean oracle: cube A (2x2x2 at origin) minus cube B (offset (1,1,1)).
Study Blender's Exact-solver output face structure (should be clean quads/ngons, NOT triangles)."""
import bpy, bmesh, sys, json
argv = sys.argv
outdir = argv[argv.index("--") + 1] if "--" in argv else "."

def add_cube(size, loc):
    bpy.ops.mesh.primitive_cube_add(size=size, location=loc)
    return bpy.context.active_object

# clean scene
bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
A = add_cube(2.0, (0,0,0))     # [-1,1]^3
B = add_cube(2.0, (1,1,1))     # [0,2]^3 -> overlap corner
mod = A.modifiers.new("bool", 'BOOLEAN')
mod.operation = 'DIFFERENCE'; mod.object = B; mod.solver = 'EXACT'
bpy.context.view_layer.objects.active = A
bpy.ops.object.modifier_apply(modifier="bool")
bpy.data.objects.remove(B, do_unlink=True)

me = A.data
bm = bmesh.new(); bm.from_mesh(me)
sizes = {}
for f in bm.faces: sizes[len(f.verts)] = sizes.get(len(f.verts),0)+1
out = {"nv": len(bm.verts), "ne": len(bm.edges), "nf": len(bm.faces), "face_sizes": sizes,
       "verts": [[round(v.co.x,5),round(v.co.y,5),round(v.co.z,5)] for v in bm.verts],
       "faces": [[v.index for v in f.verts] for f in bm.faces]}
json.dump(out, open(f"{outdir}/bool_diff_cube_ref.json","w"))
print(f"[ORACLE diff] nv={out['nv']} ne={out['ne']} nf={out['nf']} face_sizes={sizes}")
