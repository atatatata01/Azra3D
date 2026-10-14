"""sculpt_diff.py — compare an AZRA sculpt profile against a Blender oracle reference.

Voxel (density-field) sculpt and Blender's mesh sculpt use DIFFERENT representations, so a
vertex-for-vertex match is impossible in principle. The honest gate is the DEFORMATION SHAPE
SIGNATURE: the displacement profiles the oracle exports (center_dz along the stroke, cross_dy
across it, radial dr/dlat from the footprint center). This harness normalizes each profile to
its own peak and reports the shape correlation + peak-position error + normalized RMS — the
metrics that actually encode "does it FEEL like Blender's brush".

Usage:
  python sculpt_diff.py <blender_ref.json> <azra_profile.json> [--key dr_radial]

Both files must contain a top-level "profile" dict with the compared arrays. AZRA exports its
profile via the `export_sculpt_profile` test helper (voxel_sculpt.rs) in the same JSON shape.

Exit 0 always (report tool); prints max/mean/normalized deviation + shape correlation.
"""
import json, sys, math

def load(path):
    d = json.load(open(path))
    return d.get("profile", d)

def norm(a):
    m = max((abs(v) for v in a), default=0.0)
    return [v/m for v in a] if m > 1e-12 else list(a)

def corr(a, b):
    n = min(len(a), len(b))
    a, b = a[:n], b[:n]
    ma, mb = sum(a)/n, sum(b)/n
    ca = [x-ma for x in a]; cb = [x-mb for x in b]
    num = sum(x*y for x, y in zip(ca, cb))
    da = math.sqrt(sum(x*x for x in ca)); db = math.sqrt(sum(y*y for y in cb))
    return num/(da*db) if da > 1e-12 and db > 1e-12 else 0.0

def main():
    if len(sys.argv) < 3:
        print("usage: sculpt_diff.py <blender_ref.json> <azra.json> [--key NAME]"); return
    ref, azra = load(sys.argv[1]), load(sys.argv[2])
    key = "dr_radial"
    if "--key" in sys.argv:
        key = sys.argv[sys.argv.index("--key")+1]
    a = ref.get(key); b = azra.get(key)
    if a is None or b is None:
        print(f"[DIFF] key '{key}' missing (ref has {list(ref)}, azra has {list(azra)})"); return
    n = min(len(a), len(b))
    raw_max = max(abs(a[i]-b[i]) for i in range(n))
    raw_mean = sum(abs(a[i]-b[i]) for i in range(n))/n
    na, nb = norm(a), norm(b)
    nrms = math.sqrt(sum((na[i]-nb[i])**2 for i in range(n))/n)
    c = corr(a, b)
    pa = max(range(len(a)), key=lambda k: abs(a[k]))
    pb = max(range(len(b)), key=lambda k: abs(b[k]))
    print(f"[DIFF] key={key} bins={n}")
    print(f"[DIFF]   raw  max-dev={raw_max:.5f}  mean-dev={raw_mean:.5f} (world units)")
    print(f"[DIFF]   shape corr={c:.4f}  norm-RMS={nrms:.4f}  peak-bin ref={pa} azra={pb} (Δ={abs(pa-pb)})")
    verdict = "GOOD" if c > 0.9 and abs(pa-pb) <= 1 else ("FAIR" if c > 0.75 else "POOR")
    print(f"[DIFF]   verdict={verdict}")

if __name__ == "__main__":
    main()
