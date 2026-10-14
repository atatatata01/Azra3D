# occt_sample_tier2.py — FreeCAD/OCCT lightweight point clouds for AZRA Tier 2 Hausdorff
# Run: freecadcmd -c "import runpy; runpy.run_path(r'.../occt_sample_tier2.py', run_name='__main__')"
#
# Writes small refs/*_pts.json (sampled surface points), NOT full tessellation meshes.
# Commit these. Do NOT commit 50MB full mesh dumps.

import json
import os
import math

import FreeCAD
import Part

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "refs")
os.makedirs(OUT, exist_ok=True)

VERSION = FreeCAD.Version()
VERSION_STR = ".".join(str(x) for x in VERSION[:3])


def faces_summary(shape):
    c = {}
    for f in shape.Faces:
        t = f.Surface.TypeId
        if "Geom" in t:
            t = t.rsplit("Geom", 1)[-1]
        c[t] = c.get(t, 0) + 1
    return c, len(shape.Faces)


def sample_shape(shape, dev=0.008, max_verts=8000):
    """Sample points from the *actual* solid surface via tessellation.

    Do NOT sample face ParameterRange with valueAt — that hits untrimmed UV
    corners (e.g. the original cube vertex after fillet) and pollutes Hausdorff.
    """
    try:
        verts, _tris = shape.tessellate(dev)
    except Exception:
        verts = []
        for face in shape.Faces:
            try:
                vts, _ = face.tessellate(dev)
                verts.extend(vts)
            except Exception:
                pass
    pts = []
    seen = set()
    for v in verts:
        key = (round(v.x, 5), round(v.y, 5), round(v.z, 5))
        if key in seen:
            continue
        seen.add(key)
        pts.append([round(v.x, 6), round(v.y, 6), round(v.z, 6)])
        if len(pts) >= max_verts:
            break
    return pts


def emit_case(name, shape, meta=None):
    counts, total = faces_summary(shape)
    pts = sample_shape(shape, dev=0.008)
    payload = {
        "case": name,
        "freecad_version": VERSION_STR,
        "faces": counts,
        "total": total,
        "points": pts,
        "n_points": len(pts),
        "meta": meta or {},
        "coord_note": "Part.makeBox(1,1,1) = [0,1]^3. AZRA unit box is centered hx=0.5 → shift +0.5 for compare.",
    }
    path = os.path.join(OUT, name + "_pts.json")
    with open(path, "w", encoding="utf-8") as fp:
        json.dump(payload, fp, indent=1)
    print(name, counts, "pts", len(pts), "->", path)
    return payload


def box_unit():
    return Part.makeBox(1, 1, 1)


def edges_at_point(box, pt, tol=1e-6):
    out = []
    for i, e in enumerate(box.Edges):
        for v in e.Vertexes:
            p = v.Point
            if (
                abs(p.x - pt[0]) < tol
                and abs(p.y - pt[1]) < tol
                and abs(p.z - pt[2]) < tol
            ):
                out.append(i)
                break
    return out


def top_loop_indices(box):
    top = []
    for i, e in enumerate(box.Edges):
        a, b = e.Vertexes[0].Point, e.Vertexes[1].Point
        if abs(a.z - 1.0) < 1e-9 and abs(b.z - 1.0) < 1e-9:
            top.append(i)
    return top


def main():
    print("FreeCAD", VERSION_STR)
    box = box_unit()
    r = 0.2

    # ── Tier 0 / box ──
    # Edge 5: (1,1,1)->(1,0,1) top edge — matches AZRA cube_single_top_edge_matching_freecad5
    emit_case(
        "single_edge_r02",
        box.makeFillet(r, [box.Edges[5]]),
        {"tier": "0.1", "edges": [5], "mid_occt": [1.0, 0.5, 1.0]},
    )

    # Linear variable radius (makeFillet(r0, r1, edges)) — oracle for slanted setback.
    # Hole-only Hausdorff catches uniform max-r setback G0 crack.
    emit_case(
        "single_edge_linear_r005_r015",
        box.makeFillet(0.05, 0.15, [box.Edges[5]]),
        {
            "tier": "3.1",
            "edges": [5],
            "r0": 0.05,
            "r1": 0.15,
            "mid_occt": [1.0, 0.5, 1.0],
            "edge5_ends": [[1.0, 1.0, 1.0], [1.0, 0.0, 1.0]],
        },
    )

    e111 = edges_at_point(box, (1.0, 1.0, 1.0))
    print("edges at (1,1,1)", e111)
    # two tops at (1,1,1): exclude vertical if possible
    tops_at = [
        i
        for i in e111
        if abs(box.Edges[i].Vertexes[0].Point.z - box.Edges[i].Vertexes[1].Point.z) < 1e-9
    ]
    if len(tops_at) >= 2:
        two = tops_at[:2]
    else:
        two = e111[:2]
    emit_case(
        "two_edges_at_vertex",
        box.makeFillet(r, [box.Edges[i] for i in two]),
        {"tier": "0.5", "edges": two, "vertex": [1, 1, 1]},
    )

    emit_case(
        "three_edges_at_vertex",
        box.makeFillet(r, [box.Edges[i] for i in e111]),
        {"tier": "0.3", "edges": e111, "vertex": [1, 1, 1]},
    )

    top = top_loop_indices(box)
    print("top loop", top)
    emit_case(
        "top_loop_r02",
        box.makeFillet(r, [box.Edges[i] for i in top]),
        {"tier": "0.5/loop", "edges": top},
    )

    # all edges small mesh sample only (not 50MB full tess)
    emit_case(
        "all_edges_r02",
        box.makeFillet(r, box.Edges),
        {"tier": "all", "edges": "all"},
    )

    # L-bracket re-entrant sample
    base = Part.makeBox(2.0, 1.0, 0.3)
    upright = Part.makeBox(0.3, 1.0, 2.0)
    lb = base.fuse(upright).removeSplitter()
    # Print verts near re-entrant region for debugging
    for i, v in enumerate(lb.Vertexes):
        p = v.Point
        if p.x < 0.5 and p.z < 0.5:
            print("lb vert", i, (round(p.x, 3), round(p.y, 3), round(p.z, 3)))
    # Inner re-entrant: edges with mid near x=0.3,z=0.3
    lb_e = []
    for i, e in enumerate(lb.Edges):
        a, b = e.Vertexes[0].Point, e.Vertexes[1].Point
        mid = FreeCAD.Vector((a.x + b.x) * 0.5, (a.y + b.y) * 0.5, (a.z + b.z) * 0.5)
        if abs(mid.x - 0.3) < 0.15 and abs(mid.z - 0.3) < 0.15:
            lb_e.append(i)
    # Also any edge with endpoint exactly at re-entrant corner candidates
    for i, e in enumerate(lb.Edges):
        for v in e.Vertexes:
            p = v.Point
            if abs(p.x - 0.3) < 1e-4 and abs(p.z - 0.3) < 1e-4:
                if i not in lb_e:
                    lb_e.append(i)
    print("L-bracket edges near re-entrant", lb_e)
    if len(lb_e) >= 2:
        try:
            emit_case(
                "lbracket_two_edges",
                lb.makeFillet(0.12, [lb.Edges[i] for i in lb_e[:2]]),
                {"tier": "1.2", "solid": "l_bracket", "edges": lb_e[:2]},
            )
        except Exception as ex:
            print("FAIL lbracket_two_edges", ex)

    # OnSame blend-only sample: FreeCAD edges 4 (convex vertical) + 6 (concave re-entrant)
    # at vertex (0.3,0,0.3). Cylinder+BSpline faces only — planar faces omit for Hausdorff.
    try:
        fil_onsame = lb.makeFillet(0.12, [lb.Edges[4], lb.Edges[6]])
        pts = []
        seen = set()
        for f in fil_onsame.Faces:
            tid = f.Surface.TypeId
            if "Cylinder" not in tid and "BSpline" not in tid:
                continue
            try:
                verts, _ = f.tessellate(0.006)
            except Exception:
                continue
            for v in verts:
                key = (round(v.x, 5), round(v.y, 5), round(v.z, 5))
                if key in seen:
                    continue
                seen.add(key)
                pts.append([round(v.x, 6), round(v.y, 6), round(v.z, 6)])
        path = os.path.join(OUT, "lbracket_onsame_blend_pts.json")
        with open(path, "w", encoding="utf-8") as fp:
            json.dump(
                {
                    "case": "lbracket_onsame_blend",
                    "freecad_version": VERSION_STR,
                    "faces": {"Cylinder": 2, "BSplineSurface": 1},
                    "total": 3,
                    "points": pts,
                    "n_points": len(pts),
                    "meta": {
                        "tier": "1.2",
                        "solid": "l_bracket",
                        "edges": [4, 6],
                        "r": 0.12,
                        "note": "Cylinder+BSpline only for OnSame Hausdorff",
                        "vertex": [0.3, 0.0, 0.3],
                    },
                    "coord_note": "FreeCAD L-bracket Z-up. AZRA faces use same frame (no cube shift).",
                },
                fp,
                indent=1,
            )
        print("lbracket_onsame_blend pts", len(pts), "->", path)
    except Exception as ex:
        print("FAIL lbracket_onsame_blend", ex)

    print("done tier2 samples")


if __name__ == "__main__":
    main()
