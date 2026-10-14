"""Faithful simulation of Blender's make_cube_corner_adj_vmesh (unit octant, circular profile)
to pin down the exact grid positions before porting to Rust. Compare against the rc3.json oracle
inner-ring direction (0.47826, 0.47826, 0.73657) for nseg=3.
"""
import math
import numpy as np

def norm(v): return v / np.linalg.norm(v)

# ---- grid store: canonical dict keyed (i,j,k) at level ns ----
class VMesh:
    def __init__(self, n, ns):
        self.n = n; self.ns = ns; self.d = {}
    def canon(self, i, j, k):
        n, ns = self.n, self.ns
        ns2 = ns // 2; odd = ns % 2
        assert 0 <= j <= ns and 0 <= k <= ns, (i, j, k, ns)
        if (not odd) and j == ns2 and k == ns2:
            return (0, ns2, ns2)
        if j <= ns2 - 1 + odd and k <= ns2:
            return (i % n, j, k)
        if k <= ns2:
            return ((i + n - 1) % n, k, ns - j)
        return ((i + 1) % n, ns - k, j)
    def get(self, i, j, k):
        return self.d[self.canon(i, j, k)]
    def set(self, i, j, k, v):
        self.d[self.canon(i, j, k)] = np.array(v, dtype=float)

# ---- profile: quarter circle between two unit axes, uniform superellipse (=angle) sampling ----
def profile_point(a, b, k, ns):
    """point k of ns segs on the great-circle arc a->b (uniform angle = Blender circle profile)"""
    ang = math.acos(np.clip(np.dot(a, b), -1, 1))
    t = k / ns
    sa = math.sin((1 - t) * ang); sb = math.sin(t * ang); s = math.sin(ang)
    return (a * sa + b * sb) / s

def sabin_gamma(n):
    return {3: 0.065247584, 4: 0.25, 5: 0.401983447, 6: 0.523423277}[n]

def cubic_subdiv(vm, bvs):
    n, ns_in = vm.n, vm.ns
    ns2_in = ns_in // 2
    ns_out = 2 * ns_in
    out = VMesh(n, ns_out)
    # --- adjust boundary EVEN verts (smooth -1/6), store into out ---
    for i in range(n):
        out.set(i, 0, 0, vm.get(i, 0, 0))
        for k in range(1, ns_in):
            co = vm.get(i, 0, k).copy()
            acc = vm.get(i, 0, k - 1) + vm.get(i, 0, k + 1) - 2.0 * co
            co = co - acc * (1.0 / 6.0)
            out.set(i, 0, 2 * k, co)
    # --- odd boundary verts: profile point then smooth vs (already written) neighbors ---
    for i in range(n):
        a, b = bvs[i], bvs[(i + 1) % n]
        for k in range(1, ns_out, 2):
            co = profile_point(a, b, k, ns_out)
            acc = out.get(i, 0, k - 1) + out.get(i, 0, k + 1) - 2.0 * co
            co = co - acc * (1.0 / 6.0)
            out.set(i, 0, k, co)
    # --- copy adjusted evens back into vm (in-place boundary update) ---
    for i in range(n):
        for k in range(0, ns_in):
            vm.set(i, 0, k, out.get(i, 0, 2 * k))
    # --- face verts ---
    for i in range(n):
        for j in range(ns2_in):
            for k in range(ns2_in):
                f = (vm.get(i, j, k) + vm.get(i, j, k + 1) + vm.get(i, j + 1, k) + vm.get(i, j + 1, k + 1)) / 4.0
                out.set(i, 2 * j + 1, 2 * k + 1, f)
    # --- vertical edge verts ---
    for i in range(n):
        for j in range(ns2_in):
            for k in range(1, ns2_in + 1):
                e = (vm.get(i, j, k) + vm.get(i, j + 1, k)
                     + out.get(i, 2 * j + 1, 2 * k - 1) + out.get(i, 2 * j + 1, 2 * k + 1)) / 4.0
                out.set(i, 2 * j + 1, 2 * k, e)
    # --- horizontal edge verts ---
    for i in range(n):
        for j in range(1, ns2_in):
            for k in range(ns2_in):
                e = (vm.get(i, j, k) + vm.get(i, j, k + 1)
                     + out.get(i, 2 * j - 1, 2 * k + 1) + out.get(i, 2 * j + 1, 2 * k + 1)) / 4.0
                out.set(i, 2 * j, 2 * k + 1, e)
    # --- interior verts (Levin: E_avg - g*F_avg + g*old), alpha=1, gamma=0.25 ---
    gam = 0.25
    for i in range(n):
        for j in range(1, ns2_in):
            for k in range(1, ns2_in + 1):
                co1 = (out.get(i, 2 * j, 2 * k - 1) + out.get(i, 2 * j, 2 * k + 1)
                       + out.get(i, 2 * j - 1, 2 * k) + out.get(i, 2 * j + 1, 2 * k)) / 4.0
                co2 = (out.get(i, 2 * j - 1, 2 * k - 1) + out.get(i, 2 * j + 1, 2 * k - 1)
                       + out.get(i, 2 * j - 1, 2 * k + 1) + out.get(i, 2 * j + 1, 2 * k + 1)) / 4.0
                out.set(i, 2 * j, 2 * k, co1 - gam * co2 + gam * vm.get(i, j, k))
    # --- center vert (Sabin gamma) ---
    gam = sabin_gamma(n)
    co1 = np.zeros(3); co2 = np.zeros(3)
    for i in range(n):
        co1 += out.get(i, ns_in, ns_in - 1)
        co2 += out.get(i, ns_in - 1, ns_in - 1) + out.get(i, ns_in - 1, ns_in + 1)
    co = co1 / n + co2 * (-gam / (2.0 * n)) + gam * vm.get(0, ns2_in, ns2_in)
    out.set(0, ns_in, ns_in, co)
    # --- final: overwrite boundary with exact profile ---
    for i in range(n):
        a, b = bvs[i], bvs[(i + 1) % n]
        for k in range(ns_out + 1):
            out.set(i, 0, k, profile_point(a, b, k, ns_out))
    return out

def interp_vmesh(vm, bvs, nseg):
    n, ns_in = vm.n, vm.ns
    ns2 = nseg // 2; odd = nseg % 2
    out = VMesh(n, nseg)
    def vmesh_fracs(i):
        fr = [0.0]
        for k in range(ns_in):
            fr.append(fr[-1] + np.linalg.norm(vm.get(i, 0, k + 1) - vm.get(i, 0, k)))
        return [f / fr[-1] for f in fr]
    def profile_fracs(i):
        a, b = bvs[i], bvs[(i + 1) % n]
        fr = [0.0]; co = a
        for k in range(nseg):
            nc = profile_point(a, b, k + 1, nseg)
            fr.append(fr[-1] + np.linalg.norm(nc - co)); co = nc
        return [f / fr[-1] for f in fr]
    def interp_range(frac, f):
        for i in range(len(frac) - 1):
            if f <= frac[i + 1]:
                rest = f - frac[i]
                r = 0.0 if rest == 0 else rest / (frac[i + 1] - frac[i])
                if i == len(frac) - 2 and r == 1.0:
                    return len(frac) - 1, 0.0
                return i, r
        return len(frac) - 1, 0.0
    for i in range(n):
        frac = vmesh_fracs(i)
        prev_frac = vmesh_fracs((i + n - 1) % n)
        new_frac = profile_fracs(i)
        prev_new_frac = profile_fracs((i + n - 1) % n)
        for j in range(0, ns2 - 1 + odd + 1):
            for k in range(0, ns2 + 1):
                k_in, restk = interp_range(frac, new_frac[k])
                k_in_prev, restkprev = interp_range(prev_frac, prev_new_frac[nseg - j])
                j_in = ns_in - k_in_prev
                restj = -restkprev
                if restj > -1e-6:
                    restj = 0.0
                else:
                    j_in -= 1
                    restj = 1.0 + restj
                if restj < 1e-6 and restk < 1e-6:
                    co = vm.get(i, j_in, k_in)
                else:
                    j0inc = 0 if (restj < 1e-6 or j_in == ns_in) else 1
                    k0inc = 0 if (restk < 1e-6 or k_in == ns_in) else 1
                    q0 = vm.get(i, j_in, k_in); q1 = vm.get(i, j_in, k_in + k0inc)
                    q2 = vm.get(i, j_in + j0inc, k_in + k0inc); q3 = vm.get(i, j_in + j0inc, k_in)
                    u, v = restk, restj
                    co = q0 * (1-u)*(1-v) + q1 * u*(1-v) + q2 * u*v + q3 * (1-u)*v
                out.set(i, j, k, co)
    if not odd:
        out.set(0, ns2, ns2, vm.get(0, ns_in // 2, ns_in // 2))
    return out

def build(nseg):
    n = 3
    bvs = [np.array([1.,0.,0.]), np.array([0.,1.,0.]), np.array([0.,0.,1.])]
    vm = VMesh(n, 2)
    for i in range(n):
        vm.set(i, 0, 0, bvs[i])
        vm.set(i, 0, 1, profile_point(bvs[i], bvs[(i+1)%n], 1, 2))
    # Blender make_cube_corner_adj_vmesh: for nseg>2 with circle profile (super_r=2 > 1.5) the
    # initial center is scaled by 1.4 (pushed outside the sphere) — the fullness of the corner.
    c0 = math.sqrt(1 / 3) * (1.4 if nseg > 2 else 1.0)
    vm.set(0, 1, 1, np.full(3, c0))
    while vm.ns < nseg:
        vm = cubic_subdiv(vm, bvs)
    if vm.ns != nseg:
        vm = interp_vmesh(vm, bvs, nseg)
    # snap to sphere
    for key in vm.d:
        vm.d[key] = norm(vm.d[key])
    return vm

vm = build(3)
inner = vm.get(0, 1, 1)
print("inner(0,1,1) =", np.round(inner, 5))
# oracle direction for the inner vert adjacent to a BV (in the real corner frame):
oracle = np.array([0.47826, 0.47826, 0.73657])
# our octant BVs are x/y/z axes; the oracle's BV analog is 'z' for vert (0.7,0.7,1.0). Sector index
# may differ — print all three inner verts and their angles:
import itertools
for i in range(3):
    p = vm.get(i, 1, 1)
    print(f"inner sector {i}: {np.round(p,5)}  angle_from_axes=",
          [round(math.degrees(math.acos(np.clip(np.dot(p, a), -1, 1))), 3) for a in [np.eye(3)[0], np.eye(3)[1], np.eye(3)[2]]])
print("oracle inner dir:", oracle, " angle_from_z =", round(math.degrees(math.acos(0.73657)), 3))
print("pole angle from axis:", round(math.degrees(math.acos(math.sqrt(1/3))), 3))
