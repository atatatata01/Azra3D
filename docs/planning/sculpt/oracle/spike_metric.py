"""spike_metric.py — apply the AZRA isolated-peak metric to a Blender counter-test dump.

Same math as core_engine/app/src/surface_validate.rs::scan_chunk_spikes (K=8, CONTRAST=3):
    d_i   = |v_i - centroid(1-ring)|
    abs   = d_i / (K * median_edge)
    ctr   = (d_i / (max ring d_j + 0.25*median_edge)) / C
    score = min(abs, ctr)      -> spike iff score > 1

  python spike_metric.py clay_carve_spike_ref.json
"""
import json, math, sys, statistics

K = 8.0
C = 3.0

def analyze(path):
    d = json.load(open(path))
    verts = d["verts"]; tris = d["tris"]
    n = len(verts)
    ssum = [[0.0, 0.0, 0.0] for _ in range(n)]
    cnt = [0]*n
    ring = [[] for _ in range(n)]
    edges = []
    def dist(a, b):
        return math.sqrt(sum((a[i]-b[i])**2 for i in range(3)))
    for t in tris:
        a, b, c = t
        pa, pb, pc = verts[a], verts[b], verts[c]
        for (i, p, q) in ((a, pb, pc), (b, pa, pc), (c, pa, pb)):
            for k in range(3):
                ssum[i][k] += p[k] + q[k]
            cnt[i] += 2
        ring[a] += [b, c]; ring[b] += [a, c]; ring[c] += [a, b]
        edges += [dist(pa, pb), dist(pb, pc), dist(pc, pa)]
    med = statistics.median(edges)
    doff = [0.0]*n
    for i in range(n):
        if cnt[i] < 4: continue
        cen = [ssum[i][k]/cnt[i] for k in range(3)]
        doff[i] = dist(verts[i], cen)
    worst_score = 0.0; worst_ratio = 0.0; spikes = 0; wi = -1
    for i in range(n):
        if doff[i] <= 0: continue
        nb = max((doff[j] for j in ring[i]), default=0.0)
        absn = doff[i] / (K * med)
        ctr = (doff[i] / (nb + 0.25*med)) / C
        score = min(absn, ctr)
        ratio = doff[i] / med
        if ratio > worst_ratio: worst_ratio = ratio
        if score > worst_score: worst_score, wi = score, i
        if score > 1.0: spikes += 1
    print(f"{d['scenario']} (Blender {d['blender_version']}): nverts={n} median_edge={med:.5f}")
    print(f"  worst ring-offset ratio {worst_ratio:.2f} x median edge")
    print(f"  worst spike SCORE {worst_score:.3f} (spike iff > 1.0)  spikes={spikes}  at vert {wi}")
    return worst_score, worst_ratio, spikes

if __name__ == "__main__":
    for p in sys.argv[1:]:
        analyze(p)
