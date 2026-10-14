"""azra_surface_profile.py — reproduce AZRA's SURFACE-mode vertex-displacement brush kernels on the
SAME rest mesh + stroke geometry the Blender oracle used, and emit the SAME `profile` JSON so
`sculpt_diff.py` can shape-compare AZRA vs Blender.

This is NOT the app binary — it faithfully transcribes the surface-brush math from
`core_engine/app/src/voxel_surface_sculpt.rs` (Pinch/Clay/Scrape/Crease/SnakeHook) + the shared
`pinch_axis_pull` (core_math/src/voxel_sculpt.rs). Voxel/mesh chunking is irrelevant to the DEFORM
SHAPE (one mesh here, no chunk borders), so the profile signature is directly comparable.

Stroke geometry copied from sculpt_ref.py: grid size 2.0 (32 subdiv), straight +X stroke
x=-0.8..0.8 at y=0, brush size 40px ≈ 0.33 world radius, strength from the ref params. Grab-family
(SnakeHook) adds a +Z drag (z_drag) that ramps along the stroke.

Usage: python azra_surface_profile.py <blender_ref.json> <brush> <out.json>
  brush in {PINCH, CLAY_STRIPS, SCRAPE, CREASE, SNAKE_HOOK}
"""
import json, sys, math

RADIUS = 0.33          # 40px ≈ 0.33 world (sculpt_ref.py comment)
NDABS  = 21
X0, X1 = -0.8, 0.8

def falloff_smooth(e):
    # Blender SMOOTH preset (hardness 0 default): 3e^2 - 2e^3, e=1-dist/r. Matches falloff_curve_only.
    e = max(0.0, min(1.0, e))
    return e*e*(3.0 - 2.0*e)

def falloff_sharp(e):
    e = max(0.0, min(1.0, e)); return e*e*e

def v_sub(a,b): return (a[0]-b[0], a[1]-b[1], a[2]-b[2])
def v_add(a,b): return (a[0]+b[0], a[1]+b[1], a[2]+b[2])
def v_scale(a,s): return (a[0]*s, a[1]*s, a[2]*s)
def v_dot(a,b): return a[0]*b[0]+a[1]*b[1]+a[2]*b[2]
def v_len(a): return math.sqrt(v_dot(a,a))
def v_cross(a,b): return (a[1]*b[2]-a[2]*b[1], a[2]*b[0]-a[0]*b[2], a[0]*b[1]-a[1]*b[0])
def v_norm(a):
    l = v_len(a)
    return (a[0]/l, a[1]/l, a[2]/l) if l > 1e-10 else (0.0,0.0,0.0)

def pinch_axis_pull(rel, normal, tangent):
    # core_math::voxel_sculpt::pinch_axis_pull — transcribed.
    tang_full = v_sub(rel, v_scale(normal, v_dot(rel, normal)))
    if v_dot(tangent, tangent) > 1e-10:
        bitan = v_norm(v_cross(tangent, normal))
        if v_dot(bitan, bitan) > 1e-10:
            return v_scale(bitan, v_dot(tang_full, bitan))
    return tang_full

def stroke_dabs(z_drag, sphere=False):
    dabs = []
    for i in range(NDABS):
        t = i/(NDABS-1)
        x = X0 + (X1-X0)*t
        if sphere:
            # sweep across the TOP of the unit sphere (project x onto the sphere surface, z up)
            z = math.sqrt(max(0.0, 1.0 - x*x))
            dabs.append((x, 0.0, z))
        else:
            z = z_drag * t   # grab-family lifts along +Z as the stroke advances
            dabs.append((x, 0.0, z))
    return dabs

def apply(brush, rest, strength, z_drag, crease_pinch, sphere=False):
    verts = [list(p) for p in rest]         # current coords (Pinch/Clay/Scrape/Crease)
    normal = (0.0, 0.0, 1.0)                 # flat grid up (surface normal / area normal)
    r = RADIUS; r2 = r*r
    stroke_tan = v_norm((X1-X0, 0.0, 0.0))   # +X net displacement (Pinch/Crease)
    dabs = stroke_dabs(z_drag, sphere)

    if brush == "SNAKE_HOOK":
        # Moving-anchor tube over PRISTINE (rest) positions. anchor = stroke start; grab_delta grows.
        anchor = (X0, 0.0, 0.0)
        end = (X1, 0.0, z_drag)
        drag = v_sub(end, anchor)
        dl = v_len(drag); axis = v_norm(drag)
        pinch_gain = 2.0*(0.5 - crease_pinch)
        out = [list(p) for p in rest]
        for i, p in enumerate(rest):
            # distance to capsule segment [anchor, anchor+drag]
            tt = max(0.0, min(1.0, v_dot(v_sub(p, anchor), drag)/(dl*dl))) if dl>1e-6 else 0.0
            closest = v_add(anchor, v_scale(drag, tt))
            dist = v_len(v_sub(p, closest))
            if dist >= r: continue
            w = falloff_smooth(1.0 - dist/r)
            if w <= 0.0: continue
            np = v_add(p, v_scale(drag, w))            # tube pull
            if abs(pinch_gain) > 1e-4:
                rel = v_sub(np, anchor)
                along = v_scale(axis, v_dot(rel, axis))
                radial = v_sub(rel, along)
                squeeze = max(-1.0, min(1.0, pinch_gain*dl/r*w))
                np = v_sub(np, v_scale(radial, squeeze))
            out[i] = list(np)
        return out

    # area plane pre-pass (Clay/Scrape/Crease): center + area normal over 1.35r sub-sphere per dab.
    # Area normal = normalized mean of the affected verts' SURFACE normals (radial on the sphere,
    # up on the flat grid) — matches the Rust `area_planes` pre-pass reading v8[3..6].
    def vert_normal(p):
        return v_norm(p) if sphere else normal
    def area_plane(dab):
        nr = 1.35*r; nr2 = nr*nr
        cx=cy=cz=0.0; nx=ny=nz=0.0; cnt=0
        for p in verts:
            if v_len(v_sub(p, dab))**2 <= nr2:
                cx+=p[0]; cy+=p[1]; cz+=p[2]
                n = vert_normal(p); nx+=n[0]; ny+=n[1]; nz+=n[2]; cnt+=1
        if cnt==0: return (dab, vert_normal(dab))
        an = v_norm((nx,ny,nz))
        if v_dot(an,an) < 1e-10: an = vert_normal(dab)
        return ((cx/cnt, cy/cnt, cz/cnt), an)

    for dab in dabs:
        s = strength
        if brush == "PINCH":
            amp = s*0.5
            for i, p in enumerate([tuple(v) for v in verts]):
                d2 = v_len(v_sub(p, dab))**2
                if d2 >= r2: continue
                w = falloff_smooth(1.0 - math.sqrt(d2)/r)
                if w<=0.0: continue
                pull = pinch_axis_pull(v_sub(p, dab), normal, stroke_tan)
                if v_dot(pull,pull) < 1e-14: continue
                k = max(-1.0, min(1.0, w*amp))     # sgn=+1
                verts[i] = list(v_sub(p, v_scale(pull, k)))
        elif brush in ("CLAY_STRIPS",):
            ac, an = area_plane(dab)
            offset = 0.20*r
            plane_d = v_dot(v_add(ac, v_scale(an, offset)), an)   # sgn=+1
            depth = 0.5*r
            for i, p in enumerate([tuple(v) for v in verts]):
                d2 = v_len(v_sub(p, dab))**2
                if d2 >= r2: continue
                w = falloff_smooth(1.0 - math.sqrt(d2)/r)
                if w<=0.0: continue
                sd = v_dot(p, an) - plane_d   # <0 = below proud plane
                if sd >= 0.0: continue
                local_z = max(0.0, min(1.0, 1.0 + sd/depth))
                z_fac = max(0.0, local_z*(1.0-local_z)*4.0)
                kk = max(0.0, min(1.0, w*z_fac*s))
                verts[i] = list(v_sub(p, v_scale(an, sd*kk)))
        elif brush == "SCRAPE":
            ac, an = area_plane(dab)
            plane_d = v_dot(ac, an)
            for i, p in enumerate([tuple(v) for v in verts]):
                d2 = v_len(v_sub(p, dab))**2
                if d2 >= r2: continue
                w = falloff_smooth(1.0 - math.sqrt(d2)/r)
                if w<=0.0: continue
                sd = v_dot(p, an) - plane_d
                if sd <= 0.0: continue
                kk = max(0.0, min(1.0, w*s*0.5))
                verts[i] = list(v_sub(p, v_scale(an, sd*kk)))
        elif brush == "CREASE":
            ac, an = area_plane(dab)
            draw_amp = s*0.12*r     # sgn=+1 groove (−normal)
            pinch_amp = s*0.5
            for i, p in enumerate([tuple(v) for v in verts]):
                d2 = v_len(v_sub(p, dab))**2
                if d2 >= r2: continue
                wc = falloff_smooth(1.0 - math.sqrt(d2)/r)
                if wc<=0.0: continue
                w = wc*wc
                np = v_sub(p, v_scale(an, draw_amp*w))
                pull = pinch_axis_pull(v_sub(p, dab), an, stroke_tan)
                if v_dot(pull,pull) > 1e-14:
                    np = v_sub(np, v_scale(pull, max(0.0, min(1.0, w*pinch_amp))))
                verts[i] = list(np)
    return verts

def profile(rest, verts, sphere):
    dz  = [verts[i][2]-rest[i][2] for i in range(len(verts))]
    dlat= [math.hypot(verts[i][0]-rest[i][0], verts[i][1]-rest[i][1]) for i in range(len(verts))]
    dy  = [verts[i][1]-rest[i][1] for i in range(len(verts))]
    NB=21; xs=[-1.0+2.0*(k/(NB-1)) for k in range(NB)]
    def barr(arr, ma, lo, hi, ga):
        acc={k:[] for k in range(NB)}
        for i,v in enumerate(rest):
            if lo<=v[ma]<=hi:
                k=int(round((v[ga]+1.0)/2.0*(NB-1)))
                if 0<=k<NB: acc[k].append(arr[i])
        return [(sum(a)/len(a) if a else 0.0) for k,a in sorted(acc.items())]
    center_dz=barr(dz,1,-0.05,0.05,0); cross_dz=barr(dz,0,-0.05,0.05,1)
    cross_dy =barr(dy,0,-0.05,0.05,1); cross_lat=barr(dlat,0,-0.05,0.05,1)
    dr=[math.sqrt(sum(c*c for c in verts[i]))-math.sqrt(sum(c*c for c in rest[i])) for i in range(len(verts))]
    NR=16
    def radial(arr, c3):
        acc={k:[] for k in range(NR)}
        for i,v in enumerate(rest):
            d=math.sqrt(sum((v[j]-c3[j])**2 for j in range(3))); k=int(d/0.06)
            if 0<=k<NR: acc[k].append(arr[i])
        return [(sum(a)/len(a) if a else 0.0) for k,a in sorted(acc.items())]
    mid3=(0.0,0.0,1.0) if sphere else (0.0,0.0,0.0)
    peak_i=max(range(NB), key=lambda k: abs(center_dz[k]))
    return {
        "dr_radial":[round(v,6) for v in radial(dr,mid3)],
        "dlat_radial":[round(v,6) for v in radial(dlat,mid3)],
        "max_dr":round(max(dr),6),"min_dr":round(min(dr),6),
        "x":[round(v,4) for v in xs],"dz":[round(v,6) for v in center_dz],
        "cross_x":[round(v,4) for v in xs],"cross_dz":[round(v,6) for v in cross_dz],
        "cross_dy":[round(v,6) for v in cross_dy],"cross_lat":[round(v,6) for v in cross_lat],
        "peak_dz":round(center_dz[peak_i],6),"peak_x":round(xs[peak_i],4),
        "max_dz":round(max(dz),6),"min_dz":round(min(dz),6),"max_lat":round(max(dlat),6),
    }

def main():
    ref_path, brush, out = sys.argv[1], sys.argv[2], sys.argv[3]
    d = json.load(open(ref_path))
    rest = [tuple(v) for v in d["rest"]]
    sphere = d.get("mesh")=="uv_sphere"
    p = d["params"]
    strength = p.get("strength", 0.5)
    z_drag = p.get("z_drag", 0.0)
    crease_pinch = p.get("crease_pinch_factor", 0.5)
    verts = apply(brush, rest, strength, z_drag, crease_pinch, sphere)
    res = {"brush": brush, "source":"azra_surface", "profile": profile(rest, verts, sphere)}
    json.dump(res, open(out,"w"))
    print(f"[AZRA-SURFACE] {brush} -> {out}  max_dz={res['profile']['max_dz']} "
          f"min_dz={res['profile']['min_dz']} max_lat={res['profile']['max_lat']}")

if __name__ == "__main__":
    main()
